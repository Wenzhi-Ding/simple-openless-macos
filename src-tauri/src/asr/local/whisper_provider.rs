//! 本地 Whisper 引擎已在减法改造中移除;这里只保留模型路径与就绪状态的
//! 纯函数助手,供渠道凭据清理逻辑(commands/credentials.rs)使用。

use std::path::{Path, PathBuf};

use anyhow::Result;

const QUANTIZED_MODEL_FILE: &str = "ggml-large-v3-turbo-q5_0.bin";

pub fn model_path_for_model(model_id: &str, model_dir: &Path) -> Result<PathBuf> {
    let id = crate::asr::local::ModelId::from_wire_id(model_id)
        .filter(|id| id.is_whisper())
        .ok_or_else(|| anyhow::anyhow!("未知的本地 Whisper 模型: {model_id}"))?;
    let file_name = id
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("本地 Whisper 模型没有文件名: {model_id}"))?;
    let path = model_path_in_dir(id, model_dir, file_name);
    Ok(path)
}

fn model_path_in_dir(id: crate::asr::local::ModelId, dir: &Path, file_name: &str) -> PathBuf {
    let path = dir.join(file_name);
    if id == crate::asr::local::ModelId::WhisperLargeV3Turbo && !path.exists() {
        let quantized = dir.join(QUANTIZED_MODEL_FILE);
        if quantized.exists() {
            return quantized;
        }
    }
    path
}

pub fn model_ready_for_model(models_root: &std::path::Path, model_id: &str) -> bool {
    let target = openless_core::LocalAsrTarget::parse(openless_core::LocalAsrRuntime::Generic, model_id)
        .ok();
    target.is_some_and(|target| {
        openless_core::directory_has_model_files(&openless_core::local_model_dir(
            models_root,
            &target,
        ))
    })
}
