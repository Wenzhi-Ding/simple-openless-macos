/// 播放听写提示音。`kind`: "start"（开始录音上行「叮咚」）/ "stop"（正常结束下行「咚叮」）。
/// 发声走宿主进程 cpal 输出流（后台线程、播完即关），不经 WebKit——渲染进程音频通道
/// 卡死且不可探测是提示音间歇性静默的根因，见 `crate::audio_cue` 模块头注。
#[tauri::command]
pub async fn play_cue(kind: String) -> Result<(), String> {
    let Some(cue_kind) = crate::audio_cue::CueKind::parse(&kind) else {
        return Err(format!("invalid cue kind: {kind}"));
    };
    crate::audio_cue::play(cue_kind);
    Ok(())
}

/// 截断正在播的提示音，不另起新音。供取消/失败退出路径与组件卸载清理使用。
#[tauri::command]
pub async fn stop_cue() -> Result<(), String> {
    crate::audio_cue::stop();
    Ok(())
}
