use super::Inner;
use std::sync::Arc;
async fn dispatch(
    inner: &Arc<Inner>,
    edge: openless_core::DictationHotkeyEdge,
) -> Result<openless_core::CliDispatchOutcome, openless_core::BackendError> {
    if !inner.backend.snapshot().running {
        inner.backend.start().await?;
    }
    inner.backend.dispatch_dictation_hotkey_edge(edge).await
}
pub(super) async fn handle_pressed_edge(
    inner: &Arc<Inner>,
    pressed_at: std::time::Instant,
    press_id: u64,
) {
    match dispatch(
        inner,
        openless_core::DictationHotkeyEdge::Pressed {
            press_id,
            at: pressed_at,
        },
    )
    .await
    {
        Ok(_) => {}
        Err(error) => log::warn!("[coord] core dictation press failed: {error}"),
    }
}
pub(super) async fn handle_released_edge(
    inner: &Arc<Inner>,
    released_at: std::time::Instant,
    press_id: u64,
) {
    match dispatch(
        inner,
        openless_core::DictationHotkeyEdge::Released {
            press_id,
            at: released_at,
        },
    )
    .await
    {
        Ok(_) => {}
        Err(error) => log::warn!("[coord] core dictation release failed: {error}"),
    }
}
pub(super) fn handle_trigger_combined(inner: &Arc<Inner>, edge: crate::hotkey::HotkeyCombinedEdge) {
    let result = inner.host.block_on(dispatch(
        inner,
        openless_core::DictationHotkeyEdge::Combined {
            press_id: edge.press_id,
            at: edge.at,
        },
    ));
    match result {
        Ok(_) => {}
        Err(error) => log::warn!("[coord] core dictation combo cancel failed: {error}"),
    }
}
pub(super) async fn cancel_active_session(inner: &Arc<Inner>) -> bool {
    let session_id = inner.backend.snapshot().dictation.session_id;
    match inner.backend.cancel_dictation(session_id).await {
        Ok(_) => true,
        Err(error) if error.code == openless_core::BackendErrorCode::InvalidState => false,
        Err(error) => {
            log::warn!("[coord] core dictation cancel failed: {error}");
            false
        }
    }
}
