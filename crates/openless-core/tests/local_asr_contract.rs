//! Local ASR contract tests (pure-local).
//!
//! 减法改造(纯本地):原文件里基于凭据 channel 创建/切换与 ModelStore
//! 下载/迁移的测试已移除;这里保留并改写仍然适用的本地回归——模型目录
//! 判定、激活事务(prepare 成功才提交偏好/失败回滚)、存储目录切换、
//! 偏好归一化与 catalog 拒绝未知模型。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

use openless_core::domains::{
    LocalAsrActivationRequest, LocalAsrModel, LocalAsrRuntimeStatus, LocalAsrSettings,
};
use openless_core::local_asr_catalog::{LocalAsrRuntime, LocalAsrTarget};
use openless_core::{
    ModelPrepareProgressSink, ModelRuntimeAdapter, NativeModelState,
    BackendConfig, BackendDependencies, BackendError, BackendErrorCode, InMemoryCredentialStore,
    OpenLessBackend, PreferencesStore,
};

fn sherpa_target(model_id: &str) -> LocalAsrTarget {
    LocalAsrTarget::parse(LocalAsrRuntime::SherpaOnnx, model_id).unwrap()
}

#[test]
fn public_local_asr_catalog_rejects_unknown_models_per_runtime() {
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::SherpaOnnx, "sense-voice-small-zh").is_ok());
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::SherpaOnnx, "gpt-4o-audio").is_err());
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::Generic, "whisper-large-v3-turbo").is_ok());
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::Generic, "sense-voice-small-zh").is_err());
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::Foundry, "whisper-small").is_ok());
    assert!(LocalAsrTarget::parse(LocalAsrRuntime::Foundry, "whisper-large").is_err());
}

#[test]
fn public_local_asr_preferences_keep_legacy_normalization_semantics() {
    assert_eq!(
        openless_core::local_asr_catalog::normalize_sherpa_language_hint("  ZH ").unwrap(),
        "zh"
    );
    assert!(openless_core::local_asr_catalog::normalize_sherpa_language_hint("zh1").is_err());
    assert_eq!(
        openless_core::local_asr_catalog::normalize_foundry_language_hint(" en ").unwrap(),
        "en"
    );
    assert!(openless_core::local_asr_catalog::normalize_foundry_language_hint("EN").is_err());
}

#[derive(Default)]
struct RecordingLocalAsrRuntime {
    invalidated: Mutex<Vec<LocalAsrRuntime>>,
    fail_release: std::sync::atomic::AtomicBool,
    fail_prepare: std::sync::atomic::AtomicBool,
    fail_preload: std::sync::atomic::AtomicBool,
    operations: Mutex<Vec<String>>,
    during_prepare: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    loaded_models: Arc<Mutex<std::collections::HashMap<LocalAsrTarget, u64>>>,
    loaded_flags: std::sync::atomic::AtomicBool,
}

impl RecordingLocalAsrRuntime {
    fn operations(&self) -> Vec<String> {
        self.operations.lock().unwrap().clone()
    }
}

impl ModelRuntimeAdapter for RecordingLocalAsrRuntime {
    fn engine_available(&self, _: LocalAsrRuntime) -> bool {
        true
    }

    fn supports_model(&self, target: &LocalAsrTarget) -> bool {
        // Generic/Foundry 引擎在本仓的 macOS 构建中不存在;sherpa 全支持。
        target.runtime == LocalAsrRuntime::SherpaOnnx
    }

    fn runtime_status(
        &self,
        settings: LocalAsrSettings,
        _: PathBuf,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        let loaded = self
            .loaded_flags
            .load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Ok(LocalAsrRuntimeStatus {
                runtime: settings.runtime,
                provider_id: settings.provider_id,
                available: true,
                loaded,
                active_model: settings.active_model.clone(),
                model_id: Some(settings.active_model),
                keep_loaded_secs: settings.keep_loaded_secs,
                endpoint: None,
                operation: None,
                error: None,
                last_error: None,
                last_prepare_ms: None,
                last_transcribe_ms: None,
                last_audio_ms: None,
            })
        })
    }

    fn inspect_native_models(
        &self,
        _targets: Vec<LocalAsrTarget>,
    ) -> BoxFuture<'static, Result<Vec<NativeModelState>, BackendError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn delete_native_model(
        &self,
        _target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        Box::pin(async {
            Err(BackendError::new(
                BackendErrorCode::Unsupported,
                "sherpa models are deleted from their local directory",
            ))
        })
    }

    fn prepare(
        &self,
        target: LocalAsrTarget,
        _: openless_core::local_asr_catalog::FoundryRuntimeSource,
        _: PathBuf,
        _progress: ModelPrepareProgressSink,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        if let Some(update) = self.during_prepare.lock().unwrap().take() {
            update();
        }
        self.operations
            .lock()
            .unwrap()
            .push(format!("prepare:{}", target.model_id()));
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
        self.loaded_models
            .lock()
            .unwrap()
            .insert(target.clone(), 1);
        Box::pin(async move { Ok(target.model_id().to_string()) })
    }

    fn cancel_prepare(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations.lock().unwrap().push("cancel_prepare".into());
        Box::pin(async move {
            let _ = runtime;
            Ok(())
        })
    }

    fn release(&self, runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations.lock().unwrap().push(format!("release:{runtime:?}"));
        if self
            .fail_release
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(BackendError::new(
                    BackendErrorCode::Platform,
                    "fixture release failed",
                ))
            });
        }
        Box::pin(async { Ok(()) })
    }

    fn preload(
        &self,
        target: LocalAsrTarget,
        _: PathBuf,
        _: String,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("preload:{}", target.model_id()));
        if self
            .fail_preload
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(BackendError::new(
                    BackendErrorCode::Platform,
                    "fixture preload failed",
                ))
            });
        }
        Box::pin(async { Ok(()) })
    }

    fn invalidate_route(&self, runtime: LocalAsrRuntime) {
        self.invalidated.lock().unwrap().push(runtime);
    }
}

fn local_asr_backend() -> (PathBuf, Arc<RecordingLocalAsrRuntime>, OpenLessBackend) {
    let data_dir = std::env::temp_dir().join(format!(
        "openless-core-local-asr-contract-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    let runtime = Arc::new(RecordingLocalAsrRuntime::default());
    let mut dependencies = BackendDependencies::unsupported();
    dependencies.local_asr_runtime = Some(runtime.clone());
    dependencies.credential_store = Arc::new(InMemoryCredentialStore::default());
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

/// 在默认模型根下放置一个「已导入」的 sherpa 模型目录。
fn import_sherpa_model(data_dir: &std::path::Path, model_id: &str) -> PathBuf {
    let dir = data_dir.join("models").join("sherpa-onnx").join(model_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.onnx"), vec![0u8; 128]).unwrap();
    dir
}

#[tokio::test]
async fn local_asr_activation_requires_imported_model_files() {
    let (data_dir, _runtime, backend) = local_asr_backend();
    // 目录存在但为空 → 拒绝激活。
    std::fs::create_dir_all(
        data_dir
            .join("models")
            .join("sherpa-onnx")
            .join("sense-voice-small-zh"),
    )
    .unwrap();
    let error = backend
        .services()
        .local_asr
        .activate(LocalAsrActivationRequest {
            target: sherpa_target("sense-voice-small-zh"),
            provider_id: "sherpa-onnx-local".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, BackendErrorCode::InvalidState);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn local_asr_activation_prepares_before_committing_preferences() {
    let (data_dir, runtime, backend) = local_asr_backend();
    import_sherpa_model(&data_dir, "sense-voice-small-zh");
    let observed = Arc::new(Mutex::new(None));
    let observer = Arc::clone(&observed);
    let preferences_path = data_dir.join("preferences.json");
    *runtime.during_prepare.lock().unwrap() = Some(Box::new(move || {
        // prepare 进行中直接读 preferences 文件:激活 provider 尚未提交。
        let preferences = PreferencesStore::open(preferences_path).unwrap();
        let captured = preferences.get().active_asr_provider.clone();
        *observer.lock().unwrap() = Some(captured);
    }));
    let result = backend
        .services()
        .local_asr
        .activate(LocalAsrActivationRequest {
            target: sherpa_target("sense-voice-small-zh"),
            provider_id: "sherpa-onnx-local".into(),
        })
        .await
        .unwrap();
    assert_eq!(result.prepared_model, "sense-voice-small-zh");
    assert_eq!(result.provider_id, "sherpa-onnx-local");
    assert_eq!(observed.lock().unwrap().as_deref(), Some("sherpa-onnx-local"));
    assert_eq!(
        backend.get_preferences().active_asr_provider,
        "sherpa-onnx-local"
    );
    assert_eq!(
        backend.get_preferences().sherpa_onnx_model,
        "sense-voice-small-zh"
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn local_asr_activation_rejects_a_provider_from_another_runtime() {
    let (data_dir, _runtime, backend) = local_asr_backend();
    import_sherpa_model(&data_dir, "sense-voice-small-zh");
    let error = backend
        .services()
        .local_asr
        .activate(LocalAsrActivationRequest {
            target: sherpa_target("sense-voice-small-zh"),
            provider_id: "local-qwen3".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, BackendErrorCode::InvalidArgument);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn local_asr_activation_rolls_back_runtime_when_prepare_fails() {
    let (data_dir, runtime, backend) = local_asr_backend();
    import_sherpa_model(&data_dir, "sense-voice-small-zh");
    runtime
        .fail_prepare
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = backend
        .services()
        .local_asr
        .activate(LocalAsrActivationRequest {
            target: sherpa_target("sense-voice-small-zh"),
            provider_id: "sherpa-onnx-local".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, BackendErrorCode::Platform);
    assert_eq!(backend.get_preferences().active_asr_provider, "sherpa-onnx-local");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn list_models_reports_installed_state_from_the_model_directory() {
    let (data_dir, _runtime, backend) = local_asr_backend();
    import_sherpa_model(&data_dir, "sense-voice-small-zh");
    let models: Vec<LocalAsrModel> = backend
        .services()
        .local_asr
        .list_models(LocalAsrRuntime::SherpaOnnx)
        .await
        .unwrap();
    assert_eq!(models.len(), 6);
    let sense_voice = models
        .iter()
        .find(|model| model.target.model_id() == "sense-voice-small-zh")
        .unwrap();
    assert!(sense_voice.installed);
    assert_eq!(sense_voice.downloaded_bytes, 128);
    assert!(models
        .iter()
        .find(|model| model.target.model_id() == "paraformer-zh")
        .is_some_and(|model| !model.installed));
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn storage_settings_follow_the_base_dir_preference() {
    let (_data_dir, _runtime, backend) = local_asr_backend();
    let initial = backend.services().local_asr.storage_settings().await.unwrap();
    assert!(initial.is_default);
    let base = std::env::temp_dir().join(format!(
        "openless-core-model-base-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let updated = backend
        .services()
        .local_asr
        .set_models_base_dir(Some(base.clone()))
        .await
        .unwrap();
    assert!(!updated.is_default);
    assert_eq!(updated.models_base_dir.as_deref(), Some(base.as_path()));
    assert_eq!(
        updated.models_root_dir,
        base.join("OpenLess").join("models")
    );
    let reverted = backend
        .services()
        .local_asr
        .set_models_base_dir(None)
        .await
        .unwrap();
    assert!(reverted.is_default);
}

#[tokio::test]
async fn delete_model_removes_the_local_directory() {
    let (data_dir, _runtime, backend) = local_asr_backend();
    let dir = import_sherpa_model(&data_dir, "sense-voice-small-zh");
    backend
        .services()
        .local_asr
        .delete_model(sherpa_target("sense-voice-small-zh"))
        .await
        .unwrap();
    assert!(!dir.exists());
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn set_keep_loaded_secs_and_language_hint_update_preferences() {
    let (_data_dir, _runtime, backend) = local_asr_backend();
    backend
        .services()
        .local_asr
        .set_keep_loaded_secs(LocalAsrRuntime::SherpaOnnx, 90)
        .await
        .unwrap();
    backend
        .services()
        .local_asr
        .set_language_hint(LocalAsrRuntime::SherpaOnnx, " zh ".into())
        .await
        .unwrap();
    let preferences = backend.get_preferences();
    assert_eq!(preferences.sherpa_onnx_keep_loaded_secs, 90);
    assert_eq!(preferences.sherpa_onnx_language_hint, "zh");
}
