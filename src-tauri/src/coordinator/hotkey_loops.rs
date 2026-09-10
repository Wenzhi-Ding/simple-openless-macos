//! Hotkey supervisor / bridge loops and shortcut wiring extracted from
//! `coordinator.rs` (behavior-preserving move; see git history).
//!
//! Functions operate on the parent `Inner`/`Coordinator` and reference
//! parent-module items via `use super::*;`. Visibility is `pub(super)` so the
//! parent `coordinator` module can call them through `use hotkey_loops::*;`.

use super::*;

//// ─────────────────────────── hotkey bridging// ───────────────────────────

/// Esc 取消专用消费线程。为什么不并入 `hotkey_bridge_loop`：bridge 为修 #468/#475
/// 的 latch 竞态把 Pressed/Released 改成了串行 block_on —— Hold 松手后 `end_session`
/// 会在 bridge 线程上同步跑完整段转写 + 润色，期间 bridge 无法 recv。若 Esc 与其同
/// 队列，取消事件只能排队等流程跑完（此时 phase 已回 Idle，cancel 变 no-op），#798
/// 在 `end_session` 里的 select! 取消赛跑永远等不到 `cancelled` 旗标 ——「转写 / 润色
/// 中按 Esc 停不下来」。独立通道 + 本线程保证 `cancel_session` 随到随执行（它是纯同步
/// 快路径：置旗标 + 清资源，不 await）。
pub(super) fn esc_cancel_bridge_loop(inner: Arc<Inner>, rx: mpsc::Receiver<()>) {
    esc_cancel_bridge_loop_with(inner, rx, |inner| {
        inner
            .host
            .block_on(super::dictation::cancel_active_session(inner));
    });
}

fn esc_cancel_bridge_loop_with(
    inner: Arc<Inner>,
    rx: mpsc::Receiver<()>,
    cancel: impl Fn(&Arc<Inner>),
) {
    while rx.recv().is_ok() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        cancel(&inner);
    }
}

/// 组合键撤销专用消费线程。撤销事件携带触发键按下代次，避免独立通道的迟到事件
/// 误取消下一次按下开启的会话。
pub(super) fn combo_abort_bridge_loop(
    inner: Arc<Inner>,
    rx: mpsc::Receiver<crate::hotkey::HotkeyCombinedEdge>,
    handler: fn(&Arc<Inner>, crate::hotkey::HotkeyCombinedEdge),
) {
    while let Ok(edge) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        handler(&inner, edge);
    }
}

pub(super) fn spawn_esc_cancel_bridge(inner: &Arc<Inner>) -> mpsc::Sender<()> {
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
    let bridge_inner = Arc::clone(inner);
    if let Err(e) = std::thread::Builder::new()
        .name("openless-esc-cancel-bridge".into())
        .spawn(move || esc_cancel_bridge_loop(bridge_inner, cancel_rx))
    {
        // 线程建不起来 = 取消通道没有消费者，Esc 取消会静默失效——这正是本 PR 想修的
        // bug 以另一种方式回归，必须留 error 日志以便排查。
        log::error!("[hotkey] esc-cancel-bridge 线程启动失败，Esc 取消将不可用: {e}");
    }
    cancel_tx
}

pub(super) fn spawn_combo_abort_bridge(
    inner: &Arc<Inner>,
    handler: fn(&Arc<Inner>, crate::hotkey::HotkeyCombinedEdge),
) -> mpsc::Sender<crate::hotkey::HotkeyCombinedEdge> {
    let (combo_tx, combo_rx) = mpsc::channel();
    let bridge_inner = Arc::clone(inner);
    std::thread::Builder::new()
        .name("openless-combo-abort-bridge".into())
        .spawn(move || combo_abort_bridge_loop(bridge_inner, combo_rx, handler))
        .ok();
    combo_tx
}

pub(super) fn hotkey_supervisor_loop(inner: Arc<Inner>) {
    let mut attempts: u32 = 0;
    let capability = HotkeyMonitor::capability();
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let target = hotkey_runtime_target(&inner);

        if inner.hotkey.lock().is_some() {
            return;
        }
        *inner.hotkey_status.lock() = HotkeyStatus {
            adapter: capability.adapter,
            state: HotkeyStatusState::Starting,
            message: Some(format!("正在安装全局快捷键监听（第 {} 次）", attempts + 1)),
            last_error: None,
        };
        let trigger = crate::shortcut_binding::legacy_modifier_trigger(&target.dictation)
            .unwrap_or(crate::types::HotkeyTrigger::Custom);
        let binding = crate::types::HotkeyBinding {
            trigger,
            mode: target.dictation_mode,
            keys: None,
        };
        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let cancel_tx = spawn_esc_cancel_bridge(&inner);
        let combo_tx = spawn_combo_abort_bridge(&inner, handle_trigger_combined);
        match HotkeyMonitor::start(binding, tx, cancel_tx, combo_tx) {
            Ok(monitor) => {
                let adapter = monitor.kind();
                *inner.hotkey.lock() = Some(monitor);
                *inner.hotkey_status.lock() = HotkeyStatus {
                    adapter,
                    state: HotkeyStatusState::Installed,
                    message: Some(format!("{} 已安装", adapter.display_name())),
                    last_error: None,
                };
                log::info!(
                    "[coord] hotkey listener installed (after {} attempt(s))",
                    attempts + 1
                );
                let inner_clone = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name("openless-hotkey-bridge".into())
                    .spawn(move || hotkey_bridge_loop(inner_clone, rx))
                    .ok();
                return;
            }
            Err(e) => {
                attempts += 1;
                let error_message = e.message.clone();
                *inner.hotkey_status.lock() = HotkeyStatus {
                    adapter: capability.adapter,
                    state: HotkeyStatusState::Failed,
                    message: Some(error_message.clone()),
                    last_error: Some(e),
                };
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[coord] hotkey listener attempt #{attempts} failed: {}; retrying in 3s",
                        error_message
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
}
// ───────────────────────────

pub(super) fn combo_hotkey_supervisor_loop(inner: Arc<Inner>) {
    let mut attempts: u32 = 0;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let target = hotkey_runtime_target(&inner);
        if crate::shortcut_binding::legacy_modifier_trigger(&target.dictation).is_some() {
            take_combo_hotkey_on_main_thread(&inner);
            inner.side_aware_combo.lock().take();
            return;
        }

        let binding = target.dictation;
        if is_unconfigured_shortcut(&binding) {
            take_combo_hotkey_on_main_thread(&inner);
            inner.side_aware_combo.lock().take();
            return;
        }

        if crate::shortcut_binding::binding_requires_side_aware_hook(&binding) {
            take_combo_hotkey_on_main_thread(&inner);
            if inner.side_aware_combo.lock().is_some() {
                return;
            }
            let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
            match crate::side_aware_combo::SideAwareComboMonitor::start(binding, tx) {
                Ok(monitor) => {
                    *inner.side_aware_combo.lock() = Some(monitor);
                    let inner_clone = Arc::clone(&inner);
                    std::thread::Builder::new()
                        .name("openless-side-combo-bridge".into())
                        .spawn(move || combo_hotkey_bridge_loop(inner_clone, rx))
                        .ok();
                    return;
                }
                Err(e) => {
                    attempts += 1;
                    if attempts <= 3 || attempts % 10 == 0 {
                        log::warn!(
                            "[coord] side-aware combo 第 {attempts} 次注册失败: {e}; 3s 后重试"
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    continue;
                }
            }
        }

        inner.side_aware_combo.lock().take();

        if inner.combo_hotkey.lock().is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
        let (init_tx, init_rx) =
            mpsc::sync_channel::<Result<ComboHotkeyMonitor, ComboHotkeyError>>(1);
        let binding_for_main = binding.clone();
        if inner
            .host
            .run_on_main_thread(move || {
                let result = ComboHotkeyMonitor::start(binding_for_main, tx);
                let _ = init_tx.send(result);
            })
            .is_err()
        {
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }

        let init_result = match init_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => r,
            Err(_) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[coord] combo hotkey 第 {attempts} 次注册超时（主线程未回执）；3s 后重试"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };

        match init_result {
            Ok(monitor) => {
                *inner.combo_hotkey.lock() = Some(monitor);
                log::info!(
                    "[coord] combo hotkey listener installed on main thread (after {} attempt(s))",
                    attempts + 1
                );
                let inner_clone = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name("openless-combo-hotkey-bridge".into())
                    .spawn(move || combo_hotkey_bridge_loop(inner_clone, rx))
                    .ok();
                return;
            }
            Err(e) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!("[coord] combo hotkey 第 {attempts} 次注册失败: {e}; 3s 后重试");
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
}

pub(super) fn combo_hotkey_bridge_loop(inner: Arc<Inner>, rx: mpsc::Receiver<ComboHotkeyEvent>) {
    let mut current_press_id = 0;
    while let Ok(evt) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        let inner_cloned = Arc::clone(&inner);
        match evt {
            // P0 #468/#475: 同 hotkey_bridge_loop —— Pressed/Released 必须串行 await，
            // 否则 latch 竞态导致 combo 快捷键二次按键失效。
            ComboHotkeyEvent::Pressed { at } => {
                current_press_id = crate::hotkey::next_press_id();
                inner.host.block_on(async {
                    handle_pressed_edge(&inner_cloned, at, current_press_id).await;
                });
            }
            ComboHotkeyEvent::Released { at } => {
                let press_id = std::mem::take(&mut current_press_id);
                inner.host.block_on(async {
                    handle_released_edge(&inner_cloned, at, press_id).await;
                });
            }
        }
    }
}

pub(super) fn action_hotkey_supervisor_loop(inner: Arc<Inner>, kind: ActionHotkeyKind) {
    let mut attempts: u32 = 0;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        // None = 用户主动停用：反注册后退出守护（由 update_action_hotkey_binding 主动路径重装）。
        let Some(binding) = action_hotkey_binding(&inner, kind) else {
            take_action_hotkey_on_main_thread(&inner, kind);
            // 对齐主 supervisor 的 exit-on-success：装/卸交给 update_action_hotkey_binding 主动路径，issue #470
            return;
        };
        if is_modifier_only_shortcut(&binding) {
            take_action_hotkey_on_main_thread(&inner, kind);
            // 对齐主 supervisor 的 exit-on-success：装/卸交给 update_action_hotkey_binding 主动路径，issue #470
            return;
        }

        if action_hotkey_slot(&inner, kind).lock().is_some() {
            // 对齐主 supervisor 的 exit-on-success：装/卸交给 update_action_hotkey_binding 主动路径，issue #470
            return;
        }

        let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
        let (init_tx, init_rx) =
            mpsc::sync_channel::<Result<ComboHotkeyMonitor, ComboHotkeyError>>(1);
        let binding_for_main = binding.clone();
        if inner
            .host
            .run_on_main_thread(move || {
                let result = ComboHotkeyMonitor::start(binding_for_main, tx);
                let _ = init_tx.send(result);
            })
            .is_err()
        {
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }

        let init_result = match init_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => r,
            Err(_) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[coord] action hotkey {kind:?} 第 {attempts} 次注册超时；3s 后重试"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };

        match init_result {
            Ok(monitor) => {
                *action_hotkey_slot(&inner, kind).lock() = Some(monitor);
                log::info!(
                    "[coord] action hotkey {kind:?} listener installed after {} attempt(s)",
                    attempts + 1
                );
                let inner_clone = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name(action_hotkey_bridge_thread_name(kind).into())
                    .spawn(move || action_hotkey_bridge_loop(inner_clone, rx, kind))
                    .ok();
                attempts = 0;
            }
            Err(e) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[coord] action hotkey {kind:?} 第 {attempts} 次注册失败: {e}; 3s 后重试"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
}

pub(super) fn action_hotkey_bridge_loop(
    inner: Arc<Inner>,
    rx: mpsc::Receiver<ComboHotkeyEvent>,
    kind: ActionHotkeyKind,
) {
    while let Ok(evt) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        if matches!(evt, ComboHotkeyEvent::Pressed { .. }) {
            handle_action_hotkey_pressed(&inner, kind);
        }
    }
}

pub(super) fn handle_action_hotkey_pressed(inner: &Arc<Inner>, kind: ActionHotkeyKind) {
    match kind {
        ActionHotkeyKind::OpenApp => inner.host.show_main_window(),
    }
}

pub(super) fn take_combo_hotkey_on_main_thread(inner: &Arc<Inner>) {
    let main_inner = Arc::clone(inner);
    if inner
        .host
        .run_on_main_thread(move || {
            main_inner.combo_hotkey.lock().take();
        })
        .is_err()
    {
        inner.combo_hotkey.lock().take();
    }
}

pub(super) fn take_action_hotkey_on_main_thread(inner: &Arc<Inner>, kind: ActionHotkeyKind) {
    let main_inner = Arc::clone(inner);
    if inner
        .host
        .run_on_main_thread(move || {
            action_hotkey_slot(&main_inner, kind).lock().take();
        })
        .is_err()
    {
        action_hotkey_slot(inner, kind).lock().take();
    }
}

pub(super) fn action_hotkey_slot(
    inner: &Arc<Inner>,
    kind: ActionHotkeyKind,
) -> &Mutex<Option<ComboHotkeyMonitor>> {
    match kind {
        ActionHotkeyKind::OpenApp => &inner.open_app_hotkey,
    }
}

pub(super) fn action_hotkey_binding(
    inner: &Arc<Inner>,
    kind: ActionHotkeyKind,
) -> Option<crate::types::ShortcutBinding> {
    let target = hotkey_runtime_target(inner);
    match kind {
        ActionHotkeyKind::OpenApp => target.open_app,
    }
}

pub(super) fn hotkey_runtime_target(inner: &Arc<Inner>) -> openless_core::HotkeyRuntimeTarget {
    inner.hotkey_runtime_target.lock().clone()
}

pub(super) fn is_modifier_only_shortcut(binding: &crate::types::ShortcutBinding) -> bool {
    binding.modifiers.is_empty()
        && (binding.primary.eq_ignore_ascii_case("shift")
            || crate::shortcut_binding::legacy_modifier_trigger(binding).is_some())
}

pub(super) fn is_unconfigured_shortcut(binding: &crate::types::ShortcutBinding) -> bool {
    binding.primary.trim().is_empty()
}

pub(super) fn action_hotkey_bridge_thread_name(kind: ActionHotkeyKind) -> &'static str {
    match kind {
        ActionHotkeyKind::OpenApp => "openless-open-app-hotkey-bridge",
    }
}

pub(super) fn hotkey_bridge_loop(inner: Arc<Inner>, rx: mpsc::Receiver<HotkeyEvent>) {
    while let Ok(evt) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            // 录制态：仅上报「录制 Fn」事件给前端（recorder 在录入态检测到 Fn 按下，
            // 浏览器不向网页层下发 Fn keydown，由 CGEventTap 上报），其余热键事件
            // 一律跳过，避免录制期间误触发听写。
            if matches!(evt, HotkeyEvent::FnRecordingPressed) {
                emit_fn_recording_pressed(&inner);
            }
            continue;
        }
        let inner_cloned = Arc::clone(&inner);
        match evt {
            // P0 #468/#475: Pressed/Released 必须串行处理，否则在 Windows 上 WH_KEYBOARD_LL
            // 边沿间隔微秒级 → 两个独立 spawn 的 task 被 work-stealing 调度器并行执行 →
            // 同一物理按键的边沿顺序错乱 → 下次按键被静默吞掉
            // (UI 关不掉 / 录音停不下来)。改为 bridge 线程内 block_on 顺序 await，
            // recv 的 FIFO 顺序就是 handler 执行顺序。
            // 注意：handle_pressed_edge / handle_released_edge 内部走 .await（含网络
            // 握手），会暂时阻塞本 bridge 线程；Hold 模式短按时 Released 会排队在 channel
            // 里直到 begin_session 完成，但 SessionPhase::Starting 已经有
            // request_stop_during_starting 兜底，begin_session 完成进 Listening 后
            // bridge 立刻 recv Released → end_session，行为正确，仅有短暂 stop 延迟。
            HotkeyEvent::Pressed { at, press_id } => {
                inner.host.block_on(async {
                    handle_pressed_edge(&inner_cloned, at, press_id).await;
                });
            }
            HotkeyEvent::Released { at, press_id } => {
                inner.host.block_on(async {
                    handle_released_edge(&inner_cloned, at, press_id).await;
                });
            }
            // Esc 取消与组合键撤销都不在此枚举里：分别走 esc_cancel_bridge_loop /
            // combo_abort_bridge_loop，避免被上面 Released → end_session /
            // Pressed → begin_session 的同步流程堵在队列里（见各自函数注释）。
            // 非录制态不会出现（CGEventTap 仅在 recording_active 时上报）；防御性忽略。
            HotkeyEvent::FnRecordingPressed => {}
        }
    }
}

/// 录制态检测到 Fn 按下 → 发事件给前端，让 ShortcutRecorder 提交 Fn 绑定。
fn emit_fn_recording_pressed(inner: &Arc<Inner>) {
    log::info!(
        "[hotkey] 录制 Fn 按下 → emit fn-shortcut-pressed (app_ready={})",
        inner.host.is_bound()
    );
    if inner.host.is_bound() {
        inner.host.emit_fn_shortcut_pressed();
    }
}

pub(super) fn reset_shortcut_held_state(inner: &Arc<Inner>) {
    if let Some(monitor) = inner.hotkey.lock().as_ref() {
        monitor.reset_held_state();
    }
    let target = hotkey_runtime_target(inner);
    if let Some(open_app) = target.open_app.as_ref() {
        if !is_modifier_only_shortcut(open_app) {
            if let Some(monitor) = inner.open_app_hotkey.lock().as_ref() {
                if let Err(e) = monitor.update_binding(open_app.clone()) {
                    log::warn!("[coord] reset open-app hotkey latch failed: {e}");
                }
            }
        }
    }
}

pub(super) async fn handle_window_hotkey_event(
    inner: &Arc<Inner>,
    event_type: String,
    key: String,
    code: String,
    repeat: bool,
) -> Result<(), String> {
    if inner.shortcut_recording_active.load(Ordering::SeqCst) {
        return Ok(());
    }
    if event_type == "keydown" && key == "Escape" {
        // Esc 路由（issue #161，减法改造后仅剩本地听写）：直接走听写取消通路。
        super::dictation::cancel_active_session(inner).await;
        return Ok(());
    }

    let _ = (inner, event_type, key, code, repeat);
    Ok(())
}
