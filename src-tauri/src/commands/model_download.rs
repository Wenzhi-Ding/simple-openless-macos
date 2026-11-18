use super::*;

/// 启动固定模型的引导下载（用户在界面显式点击后调用）。
/// 进度经 `model-download:progress` 事件上报；同键并发只接受第一个，
/// 重复调用返回错误。镜像可选官方 HuggingFace 或 hf-mirror（缺省官方）。
#[tauri::command]
pub async fn model_download_start(
    app: AppHandle,
    manager: State<'_, crate::model_download::DownloadManager>,
    key: String,
    mirror: Option<String>,
) -> Result<(), String> {
    let Some(parsed_key) = crate::model_download::ModelKey::parse(&key) else {
        return Err(format!("unknown model key: {key}"));
    };
    let parsed_mirror = crate::model_download::Mirror::parse(
        mirror.as_deref().unwrap_or("huggingface"),
    );
    manager.start(app, parsed_key, parsed_mirror)
}

/// 取消进行中的下载；`.partial` 保留，重试自动续传。
#[tauri::command]
pub async fn model_download_cancel(
    manager: State<'_, crate::model_download::DownloadManager>,
    key: String,
) -> Result<(), String> {
    match crate::model_download::ModelKey::parse(&key) {
        Some(parsed_key) => {
            manager.cancel(parsed_key);
            Ok(())
        }
        None => Err(format!("unknown model key: {key}")),
    }
}
