use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use openless_core::{
    BackendConfig, BackendDependencies, BackendError, BackendErrorCode, LocalAsrRuntimeStatus,
    ModelPrepareProgressSink, ModelRuntimeAdapter, OpenLessBackend, LOCAL_QWEN3_MLX_PROVIDER_ID,
    LOCAL_QWEN3_MODEL_ID,
};

#[derive(Default)]
struct RecordingRuntime {
    operations: Mutex<Vec<String>>,
    fail_prepare: std::sync::atomic::AtomicBool,
    loaded: std::sync::atomic::AtomicBool,
}

impl RecordingRuntime {
    fn operations(&self) -> Vec<String> {
        self.operations.lock().unwrap().clone()
    }
}

impl ModelRuntimeAdapter for RecordingRuntime {
    fn runtime_status(
        &self,
        model_dir: PathBuf,
        keep_loaded_secs: u32,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        let loaded = self.loaded.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Ok(LocalAsrRuntimeStatus {
                provider_id: LOCAL_QWEN3_MLX_PROVIDER_ID.into(),
                available: true,
                loaded,
                active_model: LOCAL_QWEN3_MODEL_ID.into(),
                model_id: loaded.then(|| LOCAL_QWEN3_MODEL_ID.into()),
                keep_loaded_secs,
                operation: None,
                error: (!model_dir.exists()).then(|| "model missing".into()),
                last_error: None,
                last_prepare_ms: None,
                last_transcribe_ms: None,
                last_audio_ms: None,
            })
        })
    }

    fn prepare(
        &self,
        model_dir: PathBuf,
        progress: ModelPrepareProgressSink,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("prepare:{}", model_dir.display()));
        if self
            .fail_prepare
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(BackendError::new(
                    BackendErrorCode::Platform,
                    "fixture prepare failed",
                ))
            });
        }
        self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            progress(openless_core::LocalAsrPrepareProgress {
                runtime: openless_core::LocalAsrRuntimeKind::Qwen3Mlx,
                phase: openless_core::LocalAsrPreparePhase::Finished,
                model_alias: LOCAL_QWEN3_MODEL_ID.into(),
                label: "ready".into(),
                percent: Some(1.0),
                error: None,
            });
            Ok(LOCAL_QWEN3_MODEL_ID.into())
        })
    }

    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations.lock().unwrap().push("cancel".into());
        Box::pin(async { Ok(()) })
    }

    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations.lock().unwrap().push("release".into());
        self.loaded
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn preload(&self, model_dir: PathBuf) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("preload:{}", model_dir.display()));
        Box::pin(async { Ok(()) })
    }
}

fn backend() -> (PathBuf, Arc<RecordingRuntime>, OpenLessBackend) {
    let data_dir = std::env::temp_dir().join(format!(
        "openless-fixed-qwen-contract-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    let runtime = Arc::new(RecordingRuntime::default());
    let mut dependencies = BackendDependencies::unsupported();
    dependencies.local_asr_runtime = Some(runtime.clone());
    let backend = OpenLessBackend::new(
        BackendConfig {
            data_dir: data_dir.clone(),
            ..BackendConfig::default()
        },
        dependencies,
    )
    .unwrap();
    (data_dir, runtime, backend)
}

fn install_fixture(data_dir: &std::path::Path) -> PathBuf {
    let dir = data_dir.join("models").join(LOCAL_QWEN3_MODEL_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), b"{}").unwrap();
    std::fs::write(dir.join("model.safetensors"), b"weights").unwrap();
    dir
}

#[test]
fn fixed_catalog_accepts_only_qwen3_asr_17b() {
    let fixed = openless_core::LocalAsrTarget::fixed();
    assert_eq!(fixed.runtime, openless_core::LocalAsrRuntime::Qwen3Mlx);
    assert_eq!(fixed.model_id(), LOCAL_QWEN3_MODEL_ID);
    assert!(openless_core::LocalAsrTarget::parse(
        openless_core::LocalAsrRuntime::Qwen3Mlx,
        LOCAL_QWEN3_MODEL_ID,
    )
    .is_ok());
    for removed in ["qwen3-asr-0.6b", "whisper-small", "sense-voice-small-zh"] {
        assert!(openless_core::LocalAsrTarget::parse(
            openless_core::LocalAsrRuntime::Qwen3Mlx,
            removed,
        )
        .is_err());
    }
}

#[tokio::test]
async fn prepare_requires_complete_fixed_model_directory() {
    let (data_dir, _runtime, backend) = backend();
    let error = backend.services().local_asr.prepare().await.unwrap_err();
    assert_eq!(error.code, BackendErrorCode::InvalidState);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn prepare_loads_then_preloads_the_same_fixed_directory() {
    let (data_dir, runtime, backend) = backend();
    let model_dir = install_fixture(&data_dir);
    assert_eq!(
        backend.services().local_asr.prepare().await.unwrap(),
        LOCAL_QWEN3_MODEL_ID
    );
    assert_eq!(
        runtime.operations(),
        vec![
            format!("prepare:{}", model_dir.display()),
            format!("preload:{}", model_dir.display()),
        ]
    );
    assert!(
        backend
            .services()
            .local_asr
            .runtime_status()
            .await
            .unwrap()
            .loaded
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn prepare_failure_never_runs_preload() {
    let (data_dir, runtime, backend) = backend();
    install_fixture(&data_dir);
    runtime
        .fail_prepare
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = backend.services().local_asr.prepare().await.unwrap_err();
    assert_eq!(error.code, BackendErrorCode::Platform);
    assert_eq!(runtime.operations().len(), 1);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn cancel_and_release_delegate_to_the_shared_runtime() {
    let (data_dir, runtime, backend) = backend();
    backend.services().local_asr.cancel_prepare().await.unwrap();
    backend.services().local_asr.release().await.unwrap();
    assert_eq!(runtime.operations(), vec!["cancel", "release"]);
    let _ = std::fs::remove_dir_all(data_dir);
}
