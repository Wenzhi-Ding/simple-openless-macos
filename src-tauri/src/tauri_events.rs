//! Core semantic-event to legacy React event bridge.
//!
//! This module is the only place where core change notifications are translated
//! into WebView event names. Window-specific state remains owned by the Tauri
//! host; the core never sees labels such as `main` or `capsule`.
//!
//! 减法改造(纯本地):QA / Less Computer / Selection Voice / Coding Agent /
//! Remote Input 的转发与胶囊竞争逻辑已随云远程功能整体移除;胶囊状态只
//! 反映本地听写管线。

use std::sync::Arc;

use openless_core::{
    BackendEventKind, CapsulePayload, CapsuleState, CapsuleStyle, DictationPhase,
    DictationStateSnapshot, EventRecvError, LocalAsrRuntimeKind, OpenLessBackend, SessionId,
};
use tauri::{AppHandle, Emitter, Manager};

pub fn start(app: AppHandle, backend: Arc<OpenLessBackend>) {
    let mut events = backend.subscribe();
    let backend_for_events = Arc::clone(&backend);
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit("backend:event", &event);
                    forward_legacy_event(&app, &backend_for_events, event.session_id, event.kind)
                        .await;
                }
                Err(EventRecvError::Lagged(dropped)) => {
                    log::warn!(
                        "[core-events] Tauri bridge lagged by {dropped} event(s); resyncing snapshots"
                    );
                    emit_resync(&app, &backend_for_events).await;
                }
                Err(EventRecvError::Closed) => break,
                Err(EventRecvError::Empty) => unreachable!("async receive never returns Empty"),
            }
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = backend.start().await {
            log::error!("[core-events] backend start failed: {error}");
        }
    });
}

/// Publish a typed semantic event through the backend instance managed by the
/// Tauri host. Platform adapters use this instead of creating a second,
/// host-only event stream.
pub(crate) fn publish<R: tauri::Runtime>(
    app: &AppHandle<R>,
    session_id: Option<SessionId>,
    kind: BackendEventKind,
) {
    let Some(backend) = app.try_state::<Arc<OpenLessBackend>>() else {
        log::warn!("[core-events] backend state unavailable while publishing adapter event");
        return;
    };
    backend.event_publisher().publish(session_id, kind);
}

async fn forward_legacy_event(
    app: &AppHandle,
    backend: &OpenLessBackend,
    session_id: Option<SessionId>,
    kind: BackendEventKind,
) {
    match kind {
        BackendEventKind::PreferencesChanged(_) => emit_preferences(app, backend),
        BackendEventKind::VocabularyChanged(_) => {
            // Legacy listeners use this only as an invalidation signal.  Do not
            // send the core revision as the old hit-count payload.
            let _ = app.emit("vocab:updated", ());
        }
        BackendEventKind::DictationStateChanged(snapshot) => {
            emit_dictation_state(app, backend, snapshot)
        }
        BackendEventKind::TranscriptDelta(_) => {}
        BackendEventKind::DictationCompleted(result) => {
            if let Some(coordinator) = app.try_state::<Arc<crate::coordinator::Coordinator>>() {
                let message = match &result.inserted {
                    openless_core::DictationInsertStatus::Inserted => "已输入",
                    openless_core::DictationInsertStatus::CopiedFallback => "已复制，请手动粘贴",
                    openless_core::DictationInsertStatus::NotRequested => "处理完成",
                };
                coordinator.present_core_capsule(CapsulePayload {
                    state: CapsuleState::Done,
                    level: 0.0,
                    elapsed_ms: result.duration_ms,
                    message: Some(message.to_string()),
                    inserted_chars: Some(
                        u32::try_from(result.polished_text.chars().count()).unwrap_or(u32::MAX),
                    ),
                    warming: false,
                    capsule_style: backend.get_preferences().capsule_style,
                });
            }
        }
        BackendEventKind::RecordingControlRequested(request) => {
            let Some(backend) = app
                .try_state::<Arc<OpenLessBackend>>()
                .map(|backend| Arc::clone(&*backend))
            else {
                return;
            };
            tauri::async_runtime::spawn(async move {
                let result = match request.action {
                    openless_core::RecordingControlAction::Stop => backend
                        .stop_dictation_session(request.session_id)
                        .await
                        .map(|_| ()),
                    openless_core::RecordingControlAction::Cancel => {
                        backend.cancel_dictation(Some(request.session_id)).await
                    }
                };
                if let Err(error) = result {
                    // InvalidState：会话已被别的路径收尾；Cancelled：空录音静默
                    // 结束。两者都是预期结果，不是自动收尾失败。
                    if !matches!(
                        error.code,
                        openless_core::BackendErrorCode::InvalidState
                            | openless_core::BackendErrorCode::Cancelled
                    ) {
                        log::warn!("[recording] automatic terminal action failed: {error}");
                    }
                }
            });
        }
        BackendEventKind::InsertFallback(fallback) => {
            if let Some(text) = fallback.copied_text {
                if let Some(coordinator) = app.try_state::<Arc<crate::coordinator::Coordinator>>() {
                    coordinator.show_core_insert_fallback(text, &fallback.reason);
                }
            }
        }
        BackendEventKind::LocalAsrPrepareProgress(progress) => {
            let event_name = match progress.runtime {
                LocalAsrRuntimeKind::Qwen3Mlx => "local-asr-prepare-progress",
            };
            let payload = serde_json::json!({
                "phase": progress.phase,
                "modelAlias": progress.model_alias,
                "label": progress.label,
                "percent": progress.percent,
                "error": progress.error,
            });
            let _ = app.emit(event_name, payload);
        }
        BackendEventKind::LocalAsrEngineChanged(status) => {
            let _ = app.emit("local-asr:engine-changed", status);
        }
        BackendEventKind::MicrophoneDevicesChanged => {
            let _ = app.emit("microphone:devices-changed", serde_json::json!({}));
        }
        BackendEventKind::Notification(notice) => {
            // 本地听写的转录提示:仅当通知属于进行中的转录会话时展示。
            let snapshot = backend.snapshot();
            if let Some(payload) = transcription_notice_payload(
                session_id,
                &notice.message,
                &snapshot.dictation,
                backend.get_preferences().capsule_style,
            ) {
                if let Some(coordinator) = app.try_state::<Arc<crate::coordinator::Coordinator>>() {
                    coordinator.present_core_capsule(payload);
                }
            }
        }
        BackendEventKind::BackendStopping => {
            emit_dictation_state(app, backend, DictationStateSnapshot::default());
        }
        // These domains either have no legacy push event or require a
        // window-specific payload that remains owned by the compatibility host.
        BackendEventKind::BackendStarted
        | BackendEventKind::HistoryChanged(_)
        | BackendEventKind::PermissionChanged(_)
        | BackendEventKind::HotkeyStatusChanged(_) => {}
    }
}

fn emit_dictation_state(
    app: &AppHandle,
    backend: &OpenLessBackend,
    snapshot: DictationStateSnapshot,
) {
    if snapshot.phase == DictationPhase::Completed {
        return;
    }
    let payload = map_dictation_state(snapshot, backend.get_preferences().capsule_style);
    if let Some(coordinator) = app.try_state::<Arc<crate::coordinator::Coordinator>>() {
        coordinator.present_core_capsule(payload);
        return;
    }
    if let Some(capsule) = app.get_webview_window("capsule") {
        let _ = capsule.emit("capsule:state", &payload);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit("capsule:state", &payload);
    }
}

fn map_dictation_state(
    snapshot: DictationStateSnapshot,
    capsule_style: CapsuleStyle,
) -> CapsulePayload {
    let state = match snapshot.phase {
        DictationPhase::Idle => CapsuleState::Idle,
        DictationPhase::Starting | DictationPhase::Recording => CapsuleState::Recording,
        DictationPhase::Transcribing => CapsuleState::Transcribing,
        DictationPhase::Polishing | DictationPhase::Inserting => CapsuleState::Polishing,
        DictationPhase::Completed => CapsuleState::Done,
        DictationPhase::Cancelled => CapsuleState::Cancelled,
        DictationPhase::Failed => CapsuleState::Error,
    };
    let message = match snapshot.phase {
        DictationPhase::Failed => Some(match snapshot.message.as_deref() {
            Some("PermissionDenied") => "请允许麦克风权限后重试".to_string(),
            Some("Busy") => "另一个语音任务正在进行".to_string(),
            Some("Cancelled") => "已取消".to_string(),
            Some("Provider") | Some("Network") | Some("Timeout") => {
                "识别或润色失败，请重试".to_string()
            }
            Some("Platform") => "录音或输入失败，请重试".to_string(),
            Some("InvalidArgument") => "输入或设置无效，请检查后重试".to_string(),
            Some("NoAudio") => "未收到有效音频，请检查麦克风输入设备后重试".to_string(),
            Some("InvalidState") => "当前状态无法完成此操作，请重试".to_string(),
            Some("Unsupported") => "当前配置或平台不支持此操作".to_string(),
            Some("Persistence") => "保存结果失败，请检查磁盘后重试".to_string(),
            Some("OutcomeUnknown") => "无法确认是否已输入，请检查目标应用".to_string(),
            Some("Internal") => "处理遇到内部错误，请重试".to_string(),
            Some(message) if !message.trim().is_empty() => message.to_string(),
            _ => "处理失败，请重试".to_string(),
        }),
        DictationPhase::Cancelled => Some("已取消".to_string()),
        _ => snapshot.message,
    };
    CapsulePayload {
        state,
        level: snapshot.level,
        elapsed_ms: snapshot.elapsed_ms,
        message,
        inserted_chars: None,
        warming: matches!(
            snapshot.phase,
            DictationPhase::Starting | DictationPhase::Recording
        ) && !snapshot.recording_ready,
        capsule_style,
    }
}

fn emit_preferences(app: &AppHandle, backend: &OpenLessBackend) {
    let preferences = backend.get_preferences();
    let _ = app.emit("prefs:changed", &preferences);
}

fn transcription_notice_payload(
    session_id: Option<SessionId>,
    message: &str,
    dictation: &DictationStateSnapshot,
    style: CapsuleStyle,
) -> Option<CapsulePayload> {
    let session_id = session_id?;
    if message.trim().is_empty() {
        return None;
    }
    if dictation.session_id != Some(session_id) || dictation.phase != DictationPhase::Transcribing {
        return None;
    }
    Some(CapsulePayload {
        state: CapsuleState::Transcribing,
        level: 0.0,
        elapsed_ms: dictation.elapsed_ms,
        message: Some(message.to_string()),
        inserted_chars: None,
        warming: false,
        capsule_style: style,
    })
}

async fn emit_resync(app: &AppHandle, backend: &OpenLessBackend) {
    emit_preferences(app, backend);
    let snapshot = backend.snapshot();
    emit_dictation_state(app, backend, snapshot.dictation.clone());
    let _ = app.emit("vocab:updated", ());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_dictation_state_maps_to_the_legacy_capsule_contract() {
        let cases = [
            (DictationPhase::Idle, CapsuleState::Idle, false),
            (DictationPhase::Starting, CapsuleState::Recording, true),
            (DictationPhase::Recording, CapsuleState::Recording, false),
            (
                DictationPhase::Transcribing,
                CapsuleState::Transcribing,
                false,
            ),
            (DictationPhase::Polishing, CapsuleState::Polishing, false),
            (DictationPhase::Inserting, CapsuleState::Polishing, false),
            (DictationPhase::Completed, CapsuleState::Done, false),
            (DictationPhase::Cancelled, CapsuleState::Cancelled, false),
            (DictationPhase::Failed, CapsuleState::Error, false),
        ];

        for (phase, expected_state, expected_warming) in cases {
            let payload = map_dictation_state(
                DictationStateSnapshot {
                    phase,
                    session_id: None,
                    elapsed_ms: 321,
                    level: 0.25,
                    message: Some("fixture".to_string()),
                    recording_ready: phase != DictationPhase::Starting,
                },
                CapsuleStyle::Classic,
            );
            assert_eq!(payload.state, expected_state);
            assert_eq!(payload.warming, expected_warming);
            assert_eq!(payload.capsule_style, CapsuleStyle::Classic);
            assert_eq!(payload.elapsed_ms, 321);
            assert_eq!(payload.level, 0.25);
            assert_eq!(
                payload.message.as_deref(),
                Some(if phase == DictationPhase::Cancelled {
                    "已取消"
                } else {
                    "fixture"
                })
            );
        }
    }

    #[test]
    fn missing_audio_failure_tells_the_user_to_check_the_microphone() {
        let payload = map_dictation_state(
            DictationStateSnapshot {
                phase: DictationPhase::Failed,
                message: Some("NoAudio".into()),
                ..DictationStateSnapshot::default()
            },
            CapsuleStyle::Classic,
        );
        assert_eq!(
            payload.message.as_deref(),
            Some("未收到有效音频，请检查麦克风输入设备后重试")
        );
    }

    #[test]
    fn internal_failure_tokens_are_not_shown_to_users() {
        for token in [
            "InvalidArgument",
            "NoAudio",
            "InvalidState",
            "Busy",
            "Cancelled",
            "PermissionDenied",
            "Unsupported",
            "Provider",
            "Persistence",
            "Platform",
            "OutcomeUnknown",
            "Internal",
        ] {
            let payload = map_dictation_state(
                DictationStateSnapshot {
                    phase: DictationPhase::Failed,
                    message: Some(token.into()),
                    ..DictationStateSnapshot::default()
                },
                CapsuleStyle::Classic,
            );
            assert_ne!(payload.message.as_deref(), Some(token), "{token}");
        }
    }
}
