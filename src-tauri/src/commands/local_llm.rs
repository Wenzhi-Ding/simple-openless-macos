use super::*;

/// 本地润色引擎（Gemma 4 GGUF）状态：直接读 Coordinator 持有的共享
/// LocalLlamaPolisher 实例 —— 与启动预载、听写润色是同一个对象，状态不会与
/// 实际加载情况分叉。没有导入/选择/路径 IPC：模型由构建固定。
#[tauri::command]
pub fn local_llm_model_status(
    coord: CoordinatorState<'_>,
) -> Result<crate::local_llm::LocalLlmEngineStatus, String> {
    Ok(coord.local_polisher().status())
}
