//! 本地 ASR 引擎入口。
//!
//! macOS uses the fixed Qwen3-ASR MLX worker. Other retained local helpers
//! are not registered as selectable fallbacks.

pub mod models;
pub mod qwen3_mlx;
pub mod sherpa;
pub mod sherpa_provider;
pub mod sherpa_runtime;
pub mod whisper_provider;

#[allow(unused_imports)]
pub use sherpa_provider::SherpaOnnxAsr;
#[allow(unused_imports)]
pub use sherpa_runtime::SherpaOnnxRuntime;

pub use models::ModelId;
pub use whisper_provider::{model_path_for_model, model_ready_for_model};

/// 本地 Qwen3-ASR 的历史 provider id;`local-qwen3-mlx` 现由 macOS 的
/// MLX Python worker 引擎实现,`local-qwen3` / `local-qwen3-c` 仍无引擎。
pub const PROVIDER_ID: &str = "local-qwen3";
pub const LOCAL_QWEN3_MLX_PROVIDER_ID: &str = "local-qwen3-mlx";
pub const LOCAL_QWEN3_C_PROVIDER_ID: &str = "local-qwen3-c";

pub const LOCAL_WHISPER_PROVIDER_ID: &str = "local-whisper";

pub fn is_local_whisper(id: &str) -> bool {
    id == LOCAL_WHISPER_PROVIDER_ID
}

pub fn is_local_qwen3(id: &str) -> bool {
    matches!(
        id,
        PROVIDER_ID | LOCAL_QWEN3_MLX_PROVIDER_ID | LOCAL_QWEN3_C_PROVIDER_ID
    )
}
