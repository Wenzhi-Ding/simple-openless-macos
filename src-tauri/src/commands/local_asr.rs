use super::*;

use openless_core::LocalAsrRuntime;

fn core_error(error: openless_core::BackendError) -> String {
    error.to_string()
}

/// 固定本地 ASR 引擎（Qwen3-ASR 1.7B MLX worker）的状态快照。
/// 这是 services 相关唯一保留的 IPC：模型/引擎由构建固定，没有导入、选择、
/// 路径与冒烟测试入口。`loading` 来自 worker 真实的拉起标志，
/// `model_present` 由宿主检查固定模型目录（config.json + safetensors）得出。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrEngineStatus {
    pub loaded: bool,
    pub model_id: Option<String>,
    pub keep_loaded_secs: u32,
    /// 引擎是否编入本构建(macOS 的 Qwen3-ASR MLX worker)。
    pub available: bool,
    /// worker 正在拉起/载模型（启动预载或懒加载进行中）。
    pub loading: bool,
    /// 固定模型目录是否已在磁盘上（不在 = 未找到，可给出明确提示）。
    pub model_present: bool,
    /// 最近一次错误(worker 启动/加载/转写失败原因),供状态卡显式展示。
    pub error: Option<String>,
}

impl LocalAsrEngineStatus {
    fn from_core(status: openless_core::LocalAsrRuntimeStatus) -> Self {
        Self {
            loaded: status.loaded,
            model_id: status.model_id,
            keep_loaded_secs: status.keep_loaded_secs,
            available: status.available,
            loading: status.operation.as_deref() == Some("loading"),
            model_present: fixed_model_present(),
            error: status.error,
        }
    }
}

fn fixed_model_present() -> bool {
    let Some(root) = crate::persistence::models_root().ok() else {
        return false;
    };
    let dir = root.join(crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID);
    if !dir.join("config.json").is_file() {
        return false;
    }
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries.filter_map(|entry| entry.ok()).any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".safetensors"))
            })
        })
        .unwrap_or(false)
}

#[tauri::command]
pub async fn local_asr_engine_status(
    backend: CoreState<'_>,
) -> Result<LocalAsrEngineStatus, String> {
    backend
        .services()
        .local_asr
        .runtime_status(LocalAsrRuntime::Generic)
        .await
        .map(LocalAsrEngineStatus::from_core)
        .map_err(core_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_maps_from_the_core_operation_field() {
        let core = openless_core::LocalAsrRuntimeStatus {
            runtime: LocalAsrRuntime::Generic,
            provider_id: "local-qwen3-mlx".into(),
            available: true,
            loaded: false,
            active_model: "qwen3-asr-1.7b".into(),
            model_id: None,
            keep_loaded_secs: 300,
            endpoint: None,
            operation: Some("loading".into()),
            error: None,
            last_error: None,
            last_prepare_ms: None,
            last_transcribe_ms: None,
            last_audio_ms: None,
        };
        let status = LocalAsrEngineStatus::from_core(core);
        assert!(status.loading);
        assert!(!status.loaded);
    }
}
