#![cfg_attr(target_os = "linux", allow(dead_code))]
//! Tauri path adapter for the framework-independent preferences store.

use anyhow::{Context, Result};
use std::sync::Arc;

use super::{data_dir, ensure_dir, fallback_store_path, PREFERENCES_FILE};
use crate::types::UserPreferences;

/// macOS 固定的本地 ASR provider:Qwen3-ASR 1.7B(MLX)。不再有 SenseVoice 回退或
/// 任何选择路径 —— 模型与 provider 由构建决定,偏好里只能是这个值。
#[cfg(target_os = "macos")]
const MACOS_FIXED_ASR_PROVIDER: &str = "local-qwen3-mlx";

pub struct PreferencesStore {
    inner: Arc<openless_core::PreferencesStore>,
}

impl PreferencesStore {
    pub fn new() -> Result<Self> {
        let directory = data_dir()?;
        ensure_dir(&directory)?;
        let inner = openless_core::PreferencesStore::open(directory.join(PREFERENCES_FILE))
            .context("open preferences store")?;
        enforce_fixed_preferences_on_open(&inner);
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            inner: Arc::new(openless_core::PreferencesStore::fallback(
                fallback_store_path("openless_prefs_fallback.json"),
            )),
        }
    }

    pub(crate) fn core(&self) -> Arc<openless_core::PreferencesStore> {
        Arc::clone(&self.inner)
    }

    pub fn get(&self) -> UserPreferences {
        let mut preferences = self.inner.get();
        enforce_fixed_preferences(&mut preferences);
        preferences
    }

    pub fn set(&self, mut preferences: UserPreferences) -> Result<()> {
        enforce_fixed_preferences(&mut preferences);
        self.inner.set(preferences).context("save preferences")
    }

    pub fn set_preserving_current_style_preferences(
        &self,
        mut preferences: UserPreferences,
    ) -> Result<()> {
        enforce_fixed_preferences(&mut preferences);
        self.inner
            .set_preserving_current_style_preferences(preferences)
            .context("save preferences while preserving style fields")
    }
}

/// 固定运行时偏好的单一归一化函数。所有会写出/读出宿主偏好文档的路径都必须
/// 经过它:wrapper 的 get/set、`set_settings` 等命令写入前的归一,以及启动时
/// 对磁盘旧值的一次性覆盖(见 [`enforce_fixed_preferences_on_open`])。
///
/// 注意 core 侧的 `OpenLessBackend` 直接读同一个 core store 实例,不经 wrapper,
/// 所以启动时必须把 store 内的状态就地归一,否则 core 读到的还是磁盘旧值。
pub(crate) fn enforce_fixed_preferences(preferences: &mut UserPreferences) {
    #[cfg(target_os = "macos")]
    {
        // ASR 固定 Qwen3-ASR 1.7B(MLX):磁盘上的 sherpa/云端残留一律归一,
        // 模型 id 也无条件写回固定值(旧逻辑只在为空时补写,会保留残留选择)。
        preferences.active_asr_provider = MACOS_FIXED_ASR_PROVIDER.to_string();
        preferences.local_asr_active_model =
            crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID.to_string();
        // 光标/屏幕 OCR 上下文强制开启:开关已从 UI 删除,旧的 false 磁盘值
        // 不得让功能悄悄失效。敏感应用/窗口归属等 fail-closed 门控仍在 core,
        // 与开关无关。授权只走设置页的用户主动按钮,启动绝不弹窗。
        preferences.cursor_context_enabled = true;
        preferences.screen_ocr_context_enabled = true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 非 macOS 未编入 MLX worker,维持既有强制 sherpa 行为。
        preferences.active_asr_provider = "sherpa-onnx-local".to_string();
    }
    // LLM/流水线/代理:与旧版一致,强制进程内 llama.cpp + 传统流水线 + 无代理。
    preferences.active_llm_provider = "local-llama".to_string();
    preferences.pipeline_mode = openless_core::shared_types::PipelineMode::Traditional;
    preferences.multimodal_pipeline_enabled = false;
    preferences.use_system_proxy = false;
    // 纯本地构建没有更新通道:自动检查更新一律关闭。
    preferences.auto_update_check = false;
}

/// 启动时把磁盘上的旧值就地归一到固定运行时(写回 core store,backend 与
/// wrapper 共享同一实例)。幂等:已是目标值时不写盘。
fn enforce_fixed_preferences_on_open(inner: &openless_core::PreferencesStore) {
    let mut preferences = inner.get();
    let before = serde_json::to_value(&preferences).ok();
    enforce_fixed_preferences(&mut preferences);
    let after = serde_json::to_value(&preferences).ok();
    if before.is_some() && before == after {
        return;
    }
    if let Err(error) = inner.set(preferences) {
        log::warn!("[preferences] enforce fixed runtime preferences failed: {error:#}");
    } else {
        log::info!("[preferences] normalized legacy preferences to the fixed local runtime");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

// ─────────────────── 配置迁移防复活测试 ───────────────────

/// 旧配置(云端 provider/多模态/系统代理)在写入时必须被强制归一到本地引擎。
#[test]
fn enforce_fixed_preferences_normalizes_cloud_config() {
    let mut preferences = UserPreferences::default();
    preferences.active_asr_provider = "volcengine".to_string();
    preferences.active_llm_provider = "ark".to_string();
    preferences.pipeline_mode = openless_core::shared_types::PipelineMode::Multimodal;
    preferences.multimodal_pipeline_enabled = true;
    preferences.use_system_proxy = true;

    enforce_fixed_preferences(&mut preferences);

    #[cfg(target_os = "macos")]
    assert_eq!(preferences.active_asr_provider, "local-qwen3-mlx");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(preferences.active_asr_provider, "sherpa-onnx-local");
    assert_eq!(preferences.active_llm_provider, "local-llama");
    assert_eq!(
        preferences.pipeline_mode,
        openless_core::shared_types::PipelineMode::Traditional
    );
    assert!(!preferences.multimodal_pipeline_enabled);
    assert!(!preferences.use_system_proxy);
}

/// 固定项在 macOS 上没有选择空间:sherpa 残留、错误的模型 id、关掉的
/// OCR/光标上下文、开着的自动更新都会被归一回固定值。
#[test]
#[cfg(target_os = "macos")]
fn enforce_fixed_preferences_pins_the_fixed_runtime() {
    let mut preferences = UserPreferences::default();
    preferences.active_asr_provider = "sherpa-onnx-local".to_string();
    preferences.local_asr_active_model = "sense-voice-small-zh".to_string();
    preferences.cursor_context_enabled = false;
    preferences.screen_ocr_context_enabled = false;
    preferences.auto_update_check = true;
    enforce_fixed_preferences(&mut preferences);
    assert_eq!(preferences.active_asr_provider, "local-qwen3-mlx");
    assert_eq!(
        preferences.local_asr_active_model,
        crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID
    );
    assert!(preferences.cursor_context_enabled);
    assert!(preferences.screen_ocr_context_enabled);
    assert!(!preferences.auto_update_check);
}

/// 读取路径同样强制:即使磁盘上的旧 JSON 里残留云端 provider(手工编辑或旧版本
/// 写入),get() 出来的偏好也不允许复活云端路径。
#[test]
fn preferences_get_never_resurrects_cloud_providers() {
    let directory = std::env::temp_dir().join(format!(
        "openless-offline-prefs-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("preferences.json");
    // 手工放置一份「云端时代」的偏好(模拟旧版本升级场景)。
    let legacy = serde_json::json!({
        "activeAsrProvider": "volcengine",
        "activeLlmProvider": "openai",
        "pipelineMode": "translate",
        "multimodalPipelineEnabled": true,
        "useSystemProxy": true,
        "hotkey": { "trigger": "rightCommand", "mode": "toggle", "keys": null },
    });
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let store = openless_core::PreferencesStore::open(&path).unwrap();
    let wrapper = PreferencesStore { inner: std::sync::Arc::new(store) };
    let preferences = wrapper.get();
    #[cfg(target_os = "macos")]
    assert_eq!(preferences.active_asr_provider, "local-qwen3-mlx");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(preferences.active_asr_provider, "sherpa-onnx-local");
    assert_eq!(preferences.active_llm_provider, "local-llama");
    assert_eq!(
        preferences.pipeline_mode,
        openless_core::shared_types::PipelineMode::Traditional
    );
    assert!(!preferences.multimodal_pipeline_enabled);
    assert!(!preferences.use_system_proxy);

    let _ = std::fs::remove_dir_all(directory);
}

/// 启动归一化直接改写 core store:backend(不经 wrapper 读 core)与 wrapper
/// 之后读到的必须是同一份固定值 —— 这是「prefs.core 绕过 wrapper」风险的
/// 覆盖测试。
#[test]
fn startup_normalization_is_visible_through_the_core_store() {
    let directory = std::env::temp_dir().join(format!(
        "openless-fixed-prefs-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("preferences.json");
    let legacy = serde_json::json!({
        "activeAsrProvider": "sherpa-onnx-local",
        "cursorContextEnabled": false,
        "screenOcrContextEnabled": false,
        "autoUpdateCheck": true,
        "hotkey": { "trigger": "rightCommand", "mode": "toggle", "keys": null },
    });
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let core_store = openless_core::PreferencesStore::open(&path).unwrap();
    enforce_fixed_preferences_on_open(&core_store);
    let direct = core_store.get();
    #[cfg(target_os = "macos")]
    {
        assert_eq!(direct.active_asr_provider, "local-qwen3-mlx");
        assert_eq!(
            direct.local_asr_active_model,
            crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID
        );
        assert!(direct.cursor_context_enabled);
        assert!(direct.screen_ocr_context_enabled);
    }
    assert!(!direct.auto_update_check);

    let _ = std::fs::remove_dir_all(directory);
}

    #[test]
    fn tauri_wrapper_and_backend_share_the_same_preferences_repository() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-shared-preferences-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let repositories = openless_core::BackendRepositories::open(&data_dir).unwrap();
        let wrapper = PreferencesStore {
            inner: Arc::clone(&repositories.preferences),
        };
        let backend = openless_core::OpenLessBackend::new_with_repositories(
            openless_core::BackendConfig {
                data_dir: data_dir.clone(),
                ..openless_core::BackendConfig::default()
            },
            openless_core::BackendDependencies::unsupported(),
            repositories,
        )
        .unwrap();

        let mut preferences = backend.get_preferences();
        preferences.microphone_device_name = "shared repository".to_string();
        crate::set_backend_preferences_for_test(&backend, preferences);

        assert_eq!(
            wrapper.get().microphone_device_name,
            "shared repository",
            "the compatibility wrapper must observe core writes without reopening the JSON store"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
