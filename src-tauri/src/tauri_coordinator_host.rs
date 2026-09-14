//! Tauri-only host operations used by the compatibility coordinator.
//!
//! The shared backend never sees this module. It owns the late-bound
//! [`tauri::AppHandle`] and keeps window, main-thread and managed-state access
//! out of the coordinator's business paths.
use crate::types::{CapsulePayload, CapsuleState, CapsuleStyle};
use parking_lot::Mutex;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
static CAPSULE_SUPPRESSED_BY_TOGGLE_LOGGED: AtomicBool = AtomicBool::new(false);
static CAPSULE_FIRST_SHOW_LOGGED: AtomicBool = AtomicBool::new(false);
static CAPSULE_NO_ACTIVATE_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);
static CAPSULE_WINDOW_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);
fn capsule_state_log_name(state: CapsuleState) -> &'static str {
    match state {
        CapsuleState::Idle => "idle",
        CapsuleState::Recording => "recording",
        CapsuleState::Transcribing => "transcribing",
        CapsuleState::Polishing => "polishing",
        CapsuleState::Done => "done",
        CapsuleState::Cancelled => "cancelled",
        CapsuleState::Error => "error",
    }
}
pub(crate) fn show_capsule_window_for_recording<R: tauri::Runtime>(
    app: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
    reassert_spaces: bool,
) {
    let needs_fallback = !show_capsule_window_no_activate(app, window, reassert_spaces);
    if needs_fallback && !CAPSULE_NO_ACTIVATE_FALLBACK_WARNED.swap(true, Ordering::SeqCst) {
        log::warn!("[capsule] no-activate show failed; falling back to window.show()");
    }
    if needs_fallback {
        if let Err(error) = window.show() {
            log::warn!("[capsule] show fallback failed: {error}");
        }
    }
}
fn show_capsule_window_no_activate<R: tauri::Runtime>(
    app: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
    reassert_spaces: bool,
) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let Ok(handle) = window.ns_window() else {
        return false;
    };
    let ns_window = handle as *mut AnyObject;
    if ns_window.is_null() {
        return false;
    }
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    const STATIONARY: usize = 1 << 4;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    const BEHAVIOR: usize = CAN_JOIN_ALL_SPACES | STATIONARY | FULL_SCREEN_AUXILIARY;
    unsafe {
        let _: () = msg_send![ns_window, setLevel : 25i64];
        if reassert_spaces {
            let current: usize = msg_send![ns_window, collectionBehavior];
            if current != BEHAVIOR {
                log::warn!(
                    "[capsule] collectionBehavior drifted to {current} (expected {BEHAVIOR}); re-registering"
                );
            }
            let low = STATIONARY | FULL_SCREEN_AUXILIARY;
            let _: () = msg_send![ns_window, setCollectionBehavior : low];
        } else {
            let _: () = msg_send![ns_window, setCollectionBehavior : BEHAVIOR];
        }
        let _: () = msg_send![ns_window, orderFrontRegardless];
    }
    if reassert_spaces {
        let app = app.clone();
        let window = window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            let _ = app.run_on_main_thread(move || {
                let Ok(handle) = window.ns_window() else {
                    return;
                };
                let ns_window = handle as *mut AnyObject;
                if ns_window.is_null() {
                    return;
                }
                unsafe {
                    let _: () = msg_send![
                        ns_window, setCollectionBehavior : BEHAVIOR
                    ];
                }
            });
        });
    }
    true
}
fn hide_capsule_window_if_present() {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapsuleWindowAction {
    PreserveFallbackCard,
    ShowCapsule,
    HideCapsule,
}
fn capsule_window_action(
    fallback_card_active: bool,
    show_capsule: bool,
    state: CapsuleState,
) -> CapsuleWindowAction {
    if fallback_card_active {
        CapsuleWindowAction::PreserveFallbackCard
    } else if show_capsule && !matches!(state, CapsuleState::Idle) {
        CapsuleWindowAction::ShowCapsule
    } else {
        CapsuleWindowAction::HideCapsule
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CapsuleLayoutState {
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: u32,
    monitor_height: u32,
    scale_bits: u64,
}
struct CapsuleWindowState {
    layout: Mutex<Option<CapsuleLayoutState>>,
    cursor_passthrough: AtomicBool,
    style: AtomicU8,
    fallback_card_visible: AtomicBool,
    fallback_presentation_id: AtomicU64,
    deferred_payload: Mutex<Option<CapsulePayload>>,
}
impl Default for CapsuleWindowState {
    fn default() -> Self {
        Self {
            layout: Mutex::new(None),
            cursor_passthrough: AtomicBool::new(true),
            style: AtomicU8::new(0),
            fallback_card_visible: AtomicBool::new(false),
            fallback_presentation_id: AtomicU64::new(0),
            deferred_payload: Mutex::new(None),
        }
    }
}
impl CapsuleWindowState {
    fn cache_style(&self, style: CapsuleStyle) {
        self.style.store(
            u8::from(matches!(style, CapsuleStyle::Classic)),
            Ordering::Relaxed,
        );
    }
    fn cached_style(&self) -> CapsuleStyle {
        match self.style.load(Ordering::Relaxed) {
            1 => CapsuleStyle::Classic,
            _ => CapsuleStyle::Siri,
        }
    }
    fn begin_fallback_card(&self) -> u64 {
        self.deferred_payload.lock().take();
        self.fallback_card_visible.store(true, Ordering::SeqCst);
        self.fallback_presentation_id
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }
    fn dismiss_fallback_card(&self) -> (bool, Option<CapsulePayload>) {
        let was_visible = self.fallback_card_visible.swap(false, Ordering::SeqCst);
        let deferred = was_visible
            .then(|| self.deferred_payload.lock().take())
            .flatten();
        (was_visible, deferred)
    }
    fn defer_if_fallback_active(&self, payload: &CapsulePayload) -> bool {
        let active = self.fallback_card_visible.load(Ordering::SeqCst);
        if active {
            *self.deferred_payload.lock() = Some(payload.clone());
        }
        active
    }
    fn active_fallback_presentation_id(&self) -> Option<u64> {
        self.fallback_card_visible
            .load(Ordering::SeqCst)
            .then(|| self.fallback_presentation_id.load(Ordering::SeqCst))
    }
    fn fallback_presentation_is_current(&self, presentation_id: u64) -> bool {
        self.active_fallback_presentation_id() == Some(presentation_id)
    }
}
/// Narrow Tauri window capability used by the compatibility coordinator.
///
/// The coordinator may schedule semantic capsule operations, but it never
/// receives an [`AppHandle`] or [`tauri::WebviewWindow`]. Keeping those handles
/// private prevents window code from becoming an accidental business API.
#[derive(Clone)]
pub(crate) struct TauriCapsuleWindow {
    app: AppHandle,
    state: Arc<CapsuleWindowState>,
}
impl TauriCapsuleWindow {
    fn window(&self) -> Option<tauri::WebviewWindow> {
        self.app.get_webview_window("capsule")
    }
    pub(crate) fn is_available_for(&self, state: CapsuleState) -> bool {
        let available = self.window().is_some();
        if !available && !CAPSULE_WINDOW_MISSING_LOGGED.swap(true, Ordering::SeqCst) {
            log::warn!(
                "[capsule] capsule webview window not found — show path skipped (state={})",
                capsule_state_log_name(state)
            );
        }
        available
    }
    pub(crate) fn run_on_main_thread<F>(&self, task: F) -> Result<(), String>
    where
        F: FnOnce(Self) + Send + 'static,
    {
        let capsule = self.clone();
        self.app
            .run_on_main_thread(move || task(capsule))
            .map_err(|error| error.to_string())
    }
    pub(crate) fn set_size(&self, width: f64, height: f64) -> tauri::Result<()> {
        if let Some(window) = self.window() {
            window.set_size(tauri::LogicalSize::new(width, height))?;
        }
        Ok(())
    }
    pub(crate) fn set_cursor_passthrough(&self, passthrough: bool) -> tauri::Result<()> {
        if let Some(window) = self.window() {
            window.set_ignore_cursor_events(passthrough)?;
            self.state
                .cursor_passthrough
                .store(passthrough, Ordering::SeqCst);
        }
        Ok(())
    }
    pub(crate) fn invalidate_layout(&self) {
        *self.state.layout.lock() = None;
    }
    pub(crate) fn hide(&self) -> tauri::Result<()> {
        if let Some(window) = self.window() {
            window.hide()?;
        }
        Ok(())
    }
    pub(crate) fn position_fallback_card(&self, width: f64, height: f64) -> tauri::Result<()> {
        let Some(window) = self.window() else {
            return Ok(());
        };
        let Some(monitor) = window.current_monitor()? else {
            return Ok(());
        };
        let scale = monitor.scale_factor();
        let size = monitor.size();
        let position = monitor.position();
        let monitor_width = size.width as f64 / scale;
        let monitor_height = size.height as f64 / scale;
        let monitor_x = position.x as f64 / scale;
        let monitor_y = position.y as f64 / scale;
        window.set_position(tauri::LogicalPosition::new(
            monitor_x + (monitor_width - width) / 2.0,
            monitor_y + monitor_height - height - 80.0,
        ))
    }
    pub(crate) fn position_capsule_bottom_center(&self) -> tauri::Result<()> {
        if let Some(window) = self.window() {
            crate::position_capsule_bottom_center(&window)?;
        }
        Ok(())
    }
    fn layout_snapshot(&self, window: &tauri::WebviewWindow) -> Option<CapsuleLayoutState> {
        {
            if let Some(mon) = crate::capsule_target_monitor(window) {
                return Some(CapsuleLayoutState {
                    monitor_x: mon.physical_x,
                    monitor_y: mon.physical_y,
                    monitor_width: mon.physical_width,
                    monitor_height: mon.physical_height,
                    scale_bits: mon.scale.to_bits(),
                });
            }
        }
        let monitor = window.current_monitor().ok().flatten()?;
        Some(CapsuleLayoutState {
            monitor_x: monitor.position().x,
            monitor_y: monitor.position().y,
            monitor_width: monitor.size().width,
            monitor_height: monitor.size().height,
            scale_bits: monitor.scale_factor().to_bits(),
        })
    }
    fn maybe_position_capsule_bottom_center(&self, window: &tauri::WebviewWindow) {
        let Some(next) = self.layout_snapshot(window) else {
            return;
        };
        if self.state.layout.lock().as_ref() == Some(&next) {
            return;
        }
        if crate::position_capsule_bottom_center(window).is_ok() {
            *self.state.layout.lock() = Some(next);
        }
    }
    pub(crate) fn show_for_recording(&self, reassert_spaces: bool) {
        if let Some(window) = self.window() {
            show_capsule_window_for_recording(&self.app, &window, reassert_spaces);
        }
    }
    pub(crate) fn apply_capsule_payload(
        &self,
        payload: &CapsulePayload,
        show_capsule: bool,
        classic_style: bool,
        reassert_spaces: bool,
    ) {
        self.state.cache_style(if classic_style {
            CapsuleStyle::Classic
        } else {
            CapsuleStyle::Siri
        });
        let Some(window) = self.window() else {
            return;
        };
        let fallback_card_active = self.state.defer_if_fallback_active(payload);
        {
            let action = capsule_window_action(fallback_card_active, show_capsule, payload.state);
            if action == CapsuleWindowAction::PreserveFallbackCard {
                log::debug!(
                    "[capsule] native window update deferred: insert fallback card owns the window"
                );
                return;
            }
            self.maybe_position_capsule_bottom_center(&window);
            {
                let interactive = classic_style
                    && action == CapsuleWindowAction::ShowCapsule
                    && matches!(
                        payload.state,
                        CapsuleState::Recording
                            | CapsuleState::Transcribing
                            | CapsuleState::Polishing
                    );
                let want_passthrough = !interactive;
                if self
                    .state
                    .cursor_passthrough
                    .swap(want_passthrough, Ordering::SeqCst)
                    != want_passthrough
                {
                    if let Err(error) = window.set_ignore_cursor_events(want_passthrough) {
                        log::warn!("[capsule] set_ignore_cursor_events failed: {error}");
                    }
                }
            }
            match action {
                CapsuleWindowAction::PreserveFallbackCard => unreachable!(),
                CapsuleWindowAction::ShowCapsule => {
                    if !CAPSULE_FIRST_SHOW_LOGGED.swap(true, Ordering::SeqCst) {
                        log::info!(
                            "[capsule] first show this session: show_capsule=true visible=true state={}",
                            capsule_state_log_name(payload.state)
                        );
                    }
                    show_capsule_window_for_recording(&self.app, &window, reassert_spaces);
                    crate::restore_main_window_key_if_active(&self.app);
                }
                CapsuleWindowAction::HideCapsule => {
                    if !show_capsule
                        && !matches!(payload.state, CapsuleState::Idle)
                        && !CAPSULE_SUPPRESSED_BY_TOGGLE_LOGGED.swap(true, Ordering::SeqCst)
                    {
                        log::info!(
                            "[capsule] suppressed by user toggle: show_capsule=false visible=true state={}",
                            capsule_state_log_name(payload.state)
                        );
                    }
                    hide_capsule_window_if_present();
                    let _ = window.hide();
                }
            }
        }
    }
    pub(crate) fn restore_main_window_key_if_active(&self) {
        crate::restore_main_window_key_if_active(&self.app);
    }
}
#[derive(Clone)]
pub(crate) struct TauriCoordinatorHost {
    app: crate::core_adapters::AppHandleSlot,
    capsule: Arc<CapsuleWindowState>,
}
impl TauriCoordinatorHost {
    /// 宿主持有的 AppHandle 槽（Coordinator 用来广播引擎状态等宿主级事件）。
    pub(crate) fn app_handle_slot(&self) -> &crate::core_adapters::AppHandleSlot {
        &self.app
    }
    pub(crate) fn new(app: crate::core_adapters::AppHandleSlot) -> Self {
        Self {
            app,
            capsule: Arc::new(CapsuleWindowState::default()),
        }
    }
    pub(crate) fn bind(&self, app: AppHandle) {
        *self.app.lock() = Some(app);
    }
    fn app(&self) -> Option<AppHandle> {
        self.app.lock().clone()
    }
    pub(crate) fn is_bound(&self) -> bool {
        self.app.lock().is_some()
    }
    pub(crate) fn capsule_window(&self) -> Option<TauriCapsuleWindow> {
        self.app().map(|app| TauriCapsuleWindow {
            app,
            state: Arc::clone(&self.capsule),
        })
    }
    pub(crate) fn cached_capsule_style(&self) -> CapsuleStyle {
        self.capsule.cached_style()
    }
    pub(crate) fn cache_capsule_style(&self, style: CapsuleStyle) {
        self.capsule.cache_style(style);
    }
    pub(crate) fn begin_insert_fallback_card(&self) -> u64 {
        self.capsule.begin_fallback_card()
    }
    pub(crate) fn dismiss_insert_fallback_card(&self) -> (bool, Option<CapsulePayload>) {
        self.capsule.dismiss_fallback_card()
    }
    pub(crate) fn defer_capsule_if_fallback_active(&self, payload: &CapsulePayload) -> bool {
        self.capsule.defer_if_fallback_active(payload)
    }
    pub(crate) fn active_insert_fallback_presentation_id(&self) -> Option<u64> {
        self.capsule.active_fallback_presentation_id()
    }
    pub(crate) fn insert_fallback_presentation_is_current(&self, presentation_id: u64) -> bool {
        self.capsule
            .fallback_presentation_is_current(presentation_id)
    }
    pub(crate) fn run_on_main_thread<F>(&self, task: F) -> Result<(), String>
    where
        F: FnOnce() + Send + 'static,
    {
        let app = self
            .app()
            .ok_or_else(|| "Tauri AppHandle is not bound".to_string())?;
        app.run_on_main_thread(task)
            .map_err(|error| error.to_string())
    }
    pub(crate) fn spawn<F>(&self, future: F) -> tauri::async_runtime::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tauri::async_runtime::spawn(future)
    }
    pub(crate) fn block_on<F: Future>(&self, future: F) -> F::Output {
        tauri::async_runtime::block_on(future)
    }
    pub(crate) fn emit_insert_fallback(&self, payload: &crate::types::InsertFallbackCardPayload) {
        if let Some(app) = self.app() {
            let _ = app.emit_to("capsule", "insert:fallback", payload);
        }
    }
    pub(crate) fn clear_insert_fallback(&self) {
        if let Some(app) = self.app() {
            let _ = app.emit_to(
                "capsule",
                "insert:fallback",
                None::<crate::types::InsertFallbackCardPayload>,
            );
        }
    }
    pub(crate) fn emit_capsule_state_to_capsule(&self, payload: &crate::types::CapsulePayload) {
        if let Some(app) = self.app() {
            let _ = app.emit_to("capsule", "capsule:state", payload);
        }
    }
    pub(crate) fn emit_capsule_state_to_main(&self, payload: &crate::types::CapsulePayload) {
        if let Some(app) = self.app() {
            let _ = app.emit_to("main", "capsule:state", payload);
        }
    }
    pub(crate) fn emit_fn_shortcut_pressed(&self) {
        if let Some(app) = self.app() {
            let _ = app.emit("fn-shortcut-pressed", ());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_card_owns_native_window_until_dismissed() {
        for state in [
            CapsuleState::Idle,
            CapsuleState::Recording,
            CapsuleState::Polishing,
            CapsuleState::Done,
        ] {
            assert_eq!(
                capsule_window_action(true, true, state),
                CapsuleWindowAction::PreserveFallbackCard
            );
        }
    }
    #[test]
    fn capsule_window_action_follows_visibility_without_fallback_card() {
        assert_eq!(
            capsule_window_action(false, true, CapsuleState::Recording),
            CapsuleWindowAction::ShowCapsule
        );
        assert_eq!(
            capsule_window_action(false, true, CapsuleState::Idle),
            CapsuleWindowAction::HideCapsule
        );
        assert_eq!(
            capsule_window_action(false, false, CapsuleState::Recording),
            CapsuleWindowAction::HideCapsule
        );
    }
}
