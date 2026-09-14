//! Focus-target capture and capsule-window presentation extracted from
//! `coordinator.rs` (behavior-preserving move).
//!
//! Frontmost-app capture and capsule state presentation. Native window
//! presentation belongs to `tauri_coordinator_host`.
use super::*;
/// 捕获用户开始 dictation 时的前台 app 标签（"localizedName (bundle.id)"），用作 LLM
/// polish/translate 的上下文前提，让模型按 app 调风格。详见 issue #116。
///
/// macOS 走 NSWorkspace.frontmostApplication（公开 API，无需额外权限）；
/// Windows 复用前台 HWND 拿窗口标题；Linux/其他平台返回 None。
pub(crate) fn capture_frontmost_app() -> Option<String> {
    match crate::focus_target::current_front_app_parts() {
        (Some(name), Some(bundle)) => Some(format!("{name} ({bundle})")),
        (Some(name), None) => Some(name),
        (None, Some(bundle)) => Some(bundle),
        (None, None) => None,
    }
}
fn defer_capsule_payload_if_fallback_active(inner: &Arc<Inner>, payload: &CapsulePayload) -> bool {
    inner.host.defer_capsule_if_fallback_active(payload)
}
/// `capsule_event_lock` 已由调用方持有的内部实现。自动隐藏路径必须能在验证 epoch
/// 后、发出 Idle 前一直持锁，才能保证旧 timer 不会盖掉刚到的新 payload。
fn emit_capsule_with_context_locked(
    inner: &Arc<Inner>,
    state: CapsuleState,
    level: f32,
    elapsed_ms: u64,
    message: Option<String>,
    inserted_chars: Option<u32>,
) -> u64 {
    let dictation = inner.backend.snapshot().dictation;
    let payload = CapsulePayload {
        state,
        level,
        elapsed_ms,
        message,
        inserted_chars,
        warming: state == CapsuleState::Recording
            && matches!(
                dictation.phase,
                openless_core::DictationPhase::Starting | openless_core::DictationPhase::Recording
            )
            && !dictation.recording_ready,
        capsule_style: inner.host.cached_capsule_style(),
    };
    emit_capsule_payload_locked(inner, payload)
}
/// Core 反馈完整进入同一个原生显示出口；包括 warming 等字段，
/// 不经过旧的“按 Host 状态重建 payload”路径。
pub(super) fn emit_core_capsule(
    inner: &Arc<Inner>,
    payload: CapsulePayload,
    expected_epoch: Option<u64>,
) -> Option<u64> {
    emit_capsule_at_epoch(
        &inner.capsule_event_lock,
        &inner.capsule_event_epoch,
        expected_epoch,
        || emit_capsule_payload_locked(inner, payload),
    )
}
fn emit_capsule_at_epoch(
    event_lock: &Mutex<()>,
    current_epoch: &AtomicU64,
    expected_epoch: Option<u64>,
    emit: impl FnOnce() -> u64,
) -> Option<u64> {
    let _event_guard = event_lock.lock();
    if expected_epoch.is_some_and(|expected| current_epoch.load(Ordering::SeqCst) != expected) {
        return None;
    }
    Some(emit())
}
pub(super) fn hide_core_capsule_if_current(inner: &Arc<Inner>, expected_epoch: u64) {
    let _ = emit_capsule_at_epoch(
        &inner.capsule_event_lock,
        &inner.capsule_event_epoch,
        Some(expected_epoch),
        || emit_capsule_with_context_locked(inner, CapsuleState::Idle, 0.0, 0, None, None),
    );
}
fn emit_capsule_payload_locked(inner: &Arc<Inner>, payload: CapsulePayload) -> u64 {
    let state = payload.state;
    let event_epoch = inner
        .capsule_event_epoch
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);
    let prev_state = inner.last_capsule_state.lock().replace(state);
    defer_capsule_payload_if_fallback_active(inner, &payload);
    let Some(capsule) = inner.host.capsule_window() else {
        return event_epoch;
    };
    let visible = !matches!(state, CapsuleState::Idle);
    let was_visible = matches!(prev_state, Some(s) if ! matches!(s, CapsuleState::Idle));
    let defer_capsule_emit = visible && !was_visible;
    let host_for_main = inner.host.clone();
    let backend_for_main = Arc::clone(&inner.backend);
    let payload_for_deferred_emit = if defer_capsule_emit {
        Some(payload.clone())
    } else {
        None
    };
    let payload_for_window = payload.clone();
    let _ = capsule.run_on_main_thread(move |capsule| {
        if !capsule.is_available_for(state) {
            return;
        }
        let preferences = backend_for_main.get_preferences();
        let show_capsule = preferences.show_capsule;
        let classic_style = matches!(preferences.capsule_style, CapsuleStyle::Classic);
        capsule.apply_capsule_payload(
            &payload_for_window,
            show_capsule,
            classic_style,
            payload_for_deferred_emit.is_some(),
        );
        if let Some(payload) = payload_for_deferred_emit.as_ref() {
            host_for_main.emit_capsule_state_to_capsule(payload);
        }
    });
    if !defer_capsule_emit {
        inner.host.emit_capsule_state_to_capsule(&payload);
    }
    inner.host.emit_capsule_state_to_main(&payload);
    event_epoch
}
#[cfg(test)]
mod epoch_tests {
    use super::*;
    #[test]
    fn queued_terminal_and_timer_cannot_hide_newer_native_feedback() {
        for initial in [CapsuleState::Polishing, CapsuleState::Error] {
            let lock = Mutex::new(());
            let epoch = AtomicU64::new(0);
            let visible = Mutex::new(CapsuleState::Idle);
            let write = |state| {
                assert!(
                    lock.try_lock().is_none(),
                    "the native write must remain under the epoch lock"
                );
                *visible.lock() = state;
                epoch.fetch_add(1, Ordering::SeqCst) + 1
            };
            let stale_epoch =
                emit_capsule_at_epoch(&lock, &epoch, None, || write(initial)).unwrap();
            {
                let _guard = lock.lock();
                write(CapsuleState::Error);
            }
            assert!(
                emit_capsule_at_epoch(&lock, &epoch, Some(stale_epoch), || write(
                    CapsuleState::Idle
                ))
                .is_none()
            );
            assert_eq!(*visible.lock(), CapsuleState::Error);
            assert_eq!(epoch.load(Ordering::SeqCst), 2);
        }
    }
}
