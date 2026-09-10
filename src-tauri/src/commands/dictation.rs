use super::*;

#[tauri::command]
pub async fn get_startup_snapshot(
    core: CoreState<'_>,
) -> Result<openless_core::StartupSnapshot, String> {
    core.start().await.map_err(|error| error.to_string())
}

async fn ensure_core_started(core: &openless_core::OpenLessBackend) -> Result<(), String> {
    if !core.snapshot().running {
        core.start().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn start_dictation(core: CoreState<'_>) -> Result<(), String> {
    ensure_core_started(&core).await?;
    core.start_dictation()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn stop_dictation(
    core: CoreState<'_>,
    _coord: CoordinatorState<'_>,
) -> Result<(), String> {
    ensure_core_started(&core).await?;
    core.stop_dictation()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_dictation(
    core: CoreState<'_>,
    coord: CoordinatorState<'_>,
) -> Result<(), String> {
    ensure_core_started(&core).await?;
    coord.cancel_active_voice().await;
    Ok(())
}

#[tauri::command]
pub async fn handle_window_hotkey_event(
    coord: CoordinatorState<'_>,
    event_type: String,
    key: String,
    code: String,
    repeat: bool,
) -> Result<(), String> {
    coord
        .handle_window_hotkey_event(event_type, key, code, repeat)
        .await
}

#[cfg(debug_assertions)]
#[tauri::command]
pub async fn inject_hotkey_click_for_dev(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.inject_hotkey_click_for_dev().await
}

/// 历史页「重新润色」：固定管线重跑一次润色，不写回历史记录。
#[tauri::command]
pub async fn repolish(core: CoreState<'_>, raw_text: String) -> Result<String, String> {
    log::info!(
        "[repolish] requested raw_chars={}",
        raw_text.chars().count()
    );
    core.services()
        .auxiliary
        .repolish(openless_core::RepolishRequest {
            raw_text,
            front_app: crate::coordinator::capture_frontmost_app(),
        })
        .await
        .map_err(|error| error.to_string())
}
