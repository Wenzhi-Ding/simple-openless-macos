//! Hotkey supervisor / bridge loops and shortcut wiring extracted from
//! `coordinator.rs` (behavior-preserving move; see git history).
//!
//! Functions operate on the parent `Inner`/`Coordinator` and reference
//! parent-module items via `use super::*;`. Visibility is `pub(super)` so the
//! parent `coordinator` module can call them through `use hotkey_loops::*;`.

use super::*;

// ─────────────────────────── hotkey bridging ───────────────────────────

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
        // Linux: 启动前检查 fcitx5 插件是否可用
        #[cfg(target_os = "linux")]
        if !crate::linux_fcitx::available() {
            *inner.hotkey_status.lock() = HotkeyStatus {
                adapter: capability.adapter,
                state: HotkeyStatusState::Failed,
                message: Some("fcitx5 插件不可用 — 请确保 fcitx5 已安装且在运行".into()),
                last_error: Some(crate::types::HotkeyInstallError {
                    code: "fcitx5_unavailable".into(),
                    message: "fcitx5 插件 DBus 接口无响应".into(),
                }),
            };
            log::warn!("[hotkey-supervisor] fcitx5 plugin unavailable, retrying...");
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
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
        #[cfg(target_os = "linux")]
        let (fcitx_tx, fcitx_binding) = (tx.clone(), binding.clone());
        let cancel_tx = spawn_esc_cancel_bridge(&inner);
        let combo_tx = spawn_combo_abort_bridge(&inner, handle_trigger_combined);
        #[cfg(target_os = "linux")]
        let combo_tx_for_fcitx = combo_tx.clone();
        match HotkeyMonitor::start(binding, tx, cancel_tx, combo_tx) {
            Ok(monitor) => {
                let adapter = monitor.kind();
                *inner.hotkey.lock() = Some(monitor);
                if let Some(monitor) = inner.hotkey.lock().as_ref() {
                    let (qa_trigger, selection_polish_trigger, translation_trigger) =
                        modifier_shortcut_triggers(&inner);
                    monitor.update_modifier_shortcuts(
                        qa_trigger,
                        selection_polish_trigger,
                        translation_trigger,
                    );
                }
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
                // Linux: 启动 fcitx5 插件信号监听作为热键源。
                #[cfg(target_os = "linux")]
                {
                    let (qa_trigger, selection_polish_trigger, translation_trigger) =
                        modifier_shortcut_triggers(&inner);
                    let custom_key = custom_dictation_key_string(&inner);
                    crate::linux_fcitx::start_dictation_signal_listener(
                        fcitx_tx,
                        combo_tx_for_fcitx,
                        fcitx_binding.clone(),
                        qa_trigger,
                        selection_polish_trigger,
                        translation_trigger,
                        custom_key,
                    );
                    if fcitx_binding.trigger == crate::types::HotkeyTrigger::Custom {
                        sync_custom_dictation_to_plugin(&inner);
                    } else {
                        crate::linux_fcitx::sync_binding_to_plugin(&fcitx_binding);
                    }
                }
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

// ─────────────────────────── QA hotkey supervisor ───────────────────────────


// ─────────────────────── Selection Polish hotkey ───────────────────────
// 选区润色为桌面（Windows-first）工作流，mobile 不注册全局热键。

#[cfg(not(mobile))]
pub(super) fn selection_polish_hotkey_supervisor_loop(inner: Arc<Inner>) {
    let mut attempts = 0_u32;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        match try_update_selection_polish_hotkey_binding(&inner) {
            Ok(()) => return,
            Err(error) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[selection-polish] hotkey registration attempt #{attempts} failed: {error}; retrying in 3s"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
}

#[cfg(not(mobile))]
pub(super) fn try_update_selection_polish_hotkey_binding(inner: &Arc<Inner>) -> Result<(), String> {
    let binding = hotkey_runtime_target(inner).selection_polish;
    let Some(binding) = binding else {
        take_selection_polish_hotkey_on_main_thread(inner);
        update_selection_polish_modifier_shortcut(inner);
        return Ok(());
    };

    if crate::shortcut_binding::legacy_modifier_trigger(&binding).is_some() {
        take_selection_polish_hotkey_on_main_thread(inner);
        update_selection_polish_modifier_shortcut(inner);
        return Ok(());
    }

    // A generic combo is registered by global-hotkey on the UI thread. It is
    // deliberately not routed through the side-aware singleton: side-specific
    // combos remain dictation-only until that monitor supports multiple owners.
    update_selection_polish_modifier_shortcut(inner);
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let inner_for_main = Arc::clone(inner);
    inner.host.run_on_main_thread(move || {
        let result = update_selection_polish_hotkey_on_main_thread(inner_for_main, binding)
            .map_err(|error| error.to_string());
        let _ = result_tx.send(result);
    })?;
    result_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "Selection Polish hotkey registration timed out".to_string())?
}

#[cfg(not(mobile))]
fn update_selection_polish_modifier_shortcut(inner: &Arc<Inner>) {
    if let Some(monitor) = inner.hotkey.lock().as_ref() {
        let (qa_trigger, selection_polish_trigger, translation_trigger) =
            modifier_shortcut_triggers(inner);
        monitor.update_modifier_shortcuts(
            qa_trigger,
            selection_polish_trigger,
            translation_trigger,
        );
    }
}

#[cfg(not(mobile))]
fn update_selection_polish_hotkey_on_main_thread(
    inner: Arc<Inner>,
    binding: crate::types::ShortcutBinding,
) -> Result<(), ComboHotkeyError> {
    if let Some(monitor) = inner.selection_polish_hotkey.lock().as_ref() {
        return monitor.update_binding(binding);
    }
    let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
    let monitor = ComboHotkeyMonitor::start(binding, tx)?;
    *inner.selection_polish_hotkey.lock() = Some(monitor);
    let bridge_inner = Arc::clone(&inner);
    std::thread::Builder::new()
        .name("openless-selection-polish-hotkey-bridge".into())
        .spawn(move || selection_polish_hotkey_bridge_loop(bridge_inner, rx))
        .map_err(|error| {
            ComboHotkeyError::RegisterFailed(format!("spawn bridge thread: {error}"))
        })?;
    Ok(())
}

#[cfg(not(mobile))]
fn selection_polish_hotkey_bridge_loop(inner: Arc<Inner>, rx: mpsc::Receiver<ComboHotkeyEvent>) {
    while let Ok(event) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        match event {
            ComboHotkeyEvent::Pressed { .. } => {
                crate::selection::prefetch_selection_workspace_capture();
                handle_selection_workspace_hotkey_pressed(&inner);
            }
            ComboHotkeyEvent::Released { .. } => {
                handle_selection_workspace_hotkey_released(&inner);
            }
        }
    }
}

#[cfg(not(mobile))]
fn handle_selection_workspace_hotkey_pressed(inner: &Arc<Inner>) {
    #[cfg(target_os = "windows")]
    if inner.backend.get_preferences().selection_voice_enabled {
        let inner_cloned = Arc::clone(inner);
        inner.host.spawn(async move {
            super::selection_voice_session::handle_selection_voice_pressed(&inner_cloned).await;
        });
        return;
    }
    let inner = Arc::clone(inner);
    let host = inner.host.clone();
    host.spawn(async move {
        let result = inner
            .backend
            .services()
            .selection
            .begin_polish(openless_core::SelectionPolishRequest {
                selected_text: None,
                mode: openless_core::PolishMode::Raw,
                instruction: None,
            })
            .await;
        match result {
            Ok(_) => match inner.backend.services().selection.snapshot().await {
                Ok(snapshot) => {
                    let message = match snapshot.phase {
                        openless_core::SelectionPhase::Preview => "已打开预览，等待确认",
                        openless_core::SelectionPhase::Completed => match snapshot.insert_outcome {
                            Some(openless_core::InsertOutcome::CopiedFallback) => {
                                "已复制结果，请手动粘贴"
                            }
                            _ => "已替换",
                        },
                        _ => return,
                    };
                    let epoch = emit_selection_polish_capsule(&inner, CapsuleState::Done, message);
                    schedule_selection_polish_capsule_idle(
                        &inner,
                        epoch,
                        CAPSULE_AUTO_HIDE_DELAY_MS,
                    );
                }
                Err(error) => {
                    log::warn!("[selection-polish] read completed snapshot failed: {error}");
                }
            },
            Err(error) => {
                log::warn!("[selection-polish] hotkey workflow failed: {error}");
                let message = match error.message.as_str() {
                    "selectionPolishNoSelection" | "selected text must not be empty" => {
                        "未选中内容"
                    }
                    "selectionPolishTargetUnavailable" => "目标输入框不可用，请重新选择",
                    "selectionPolishTargetChanged" | "selectionPolishSelectionChanged" => {
                        "选区已变化，未替换"
                    }
                    _ if error.code == openless_core::BackendErrorCode::Busy => {
                        "选区润色正在进行中"
                    }
                    _ => "润色失败，请重试",
                };
                let state = if matches!(
                    error.code,
                    openless_core::BackendErrorCode::Cancelled
                        | openless_core::BackendErrorCode::InvalidArgument
                ) {
                    CapsuleState::Cancelled
                } else {
                    CapsuleState::Error
                };
                let epoch = emit_selection_polish_capsule(&inner, state, message);
                schedule_selection_polish_capsule_idle(&inner, epoch, CAPSULE_AUTO_HIDE_DELAY_MS);
            }
        }
    });
}

#[cfg(not(mobile))]
fn handle_selection_workspace_hotkey_released(inner: &Arc<Inner>) {
    #[cfg(target_os = "windows")]
    {
        if !inner.backend.get_preferences().selection_voice_enabled {
            return;
        }
        let inner_cloned = Arc::clone(inner);
        inner.host.spawn(async move {
            super::selection_voice_session::handle_selection_voice_released(&inner_cloned).await;
        });
    }
    #[cfg(not(target_os = "windows"))]
    let _ = inner;
}

#[cfg(not(mobile))]
pub(super) fn take_selection_polish_hotkey_on_main_thread(inner: &Arc<Inner>) {
    let main_inner = Arc::clone(inner);
    if inner
        .host
        .run_on_main_thread(move || {
            main_inner.selection_polish_hotkey.lock().take();
        })
        .is_err()
    {
        inner.selection_polish_hotkey.lock().take();
    }
}

// ─────────────────────────── combo hotkey supervisor ───────────────────────────

// ─────────────────────── coding agent hotkey supervisor ───────────────────────

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
                #[cfg(target_os = "linux")]
                sync_custom_dictation_to_plugin(&inner);
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
        ActionHotkeyKind::SwitchStyle => switch_to_previous_style(inner),
        ActionHotkeyKind::OpenApp => inner.host.show_main_window(),
    }
}

/// 全局快捷键切风格后的轻量提示：用户多半在别的前台 app 里按键，不弹提示
/// 无法知道切没切成功、切到了哪个风格。复用选区润色的无焦点一行提示胶囊
/// （✓ + 文案，2s 自动隐藏，不抢焦点不挡点击）；录音中按键最多闪一帧，
/// 下一个 ~30Hz 电平帧会立即夺回胶囊显示，auto-hide timer 也会因代数失效。
#[cfg(not(mobile))]
pub(super) fn show_style_switch_capsule(inner: &Arc<Inner>, name: &str) {
    let event_epoch =
        emit_selection_polish_capsule(inner, CapsuleState::Done, format!("已切换：{name}"));
    schedule_selection_polish_capsule_idle(inner, event_epoch, CAPSULE_AUTO_HIDE_DELAY_MS);
}

pub(super) fn switch_to_previous_style(inner: &Arc<Inner>) {
    let selected = match inner.backend.activate_previous_style_pack() {
        Ok(selected) => selected,
        Err(error) => {
            log::warn!("[coord] switch style hotkey failed: {error}");
            return;
        }
    };
    let Some(selected) = selected else {
        log::info!("[coord] switch style hotkey ignored: enabled style count <= 1");
        return;
    };
    log::info!(
        "[coord] switch style hotkey changed active style pack to {}",
        selected.id
    );
    #[cfg(not(mobile))]
    show_style_switch_capsule(inner, &selected.name);
    inner.host.refresh_tray_microphone_menu();
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
        ActionHotkeyKind::SwitchStyle => &inner.switch_style_hotkey,
        ActionHotkeyKind::OpenApp => &inner.open_app_hotkey,
    }
}

pub(super) fn action_hotkey_binding(
    inner: &Arc<Inner>,
    kind: ActionHotkeyKind,
) -> Option<crate::types::ShortcutBinding> {
    let target = hotkey_runtime_target(inner);
    match kind {
        ActionHotkeyKind::SwitchStyle => target.switch_style,
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
        ActionHotkeyKind::SwitchStyle => "openless-switch-style-hotkey-bridge",
        ActionHotkeyKind::OpenApp => "openless-open-app-hotkey-bridge",
    }
}

// ─────────────────── style pack hotkeys (issue #759) ───────────────────

fn replace_style_pack_hotkey_registrations<R>(
    entries: &[crate::types::StylePackHotkey],
    registrations: &mut std::collections::HashMap<String, R>,
    mut register: impl FnMut(&crate::types::StylePackHotkey) -> Result<R, String>,
) -> Result<(), String> {
    registrations.clear();
    for entry in entries {
        match register(entry) {
            Ok(registration) => {
                registrations.insert(entry.pack_id.clone(), registration);
            }
            Err(error) => {
                registrations.clear();
                return Err(error);
            }
        }
    }
    Ok(())
}

fn style_pack_hotkey_registrations_match<R>(
    desired: &[crate::types::StylePackHotkey],
    registrations: &std::collections::HashMap<String, R>,
    binding_of: impl for<'a> Fn(&'a R) -> &'a crate::types::ShortcutBinding,
) -> bool {
    desired.len() == registrations.len()
        && desired.iter().all(|entry| {
            registrations
                .get(&entry.pack_id)
                .is_some_and(|registration| binding_of(registration) == &entry.binding)
        })
}

fn configured_style_pack_hotkeys(inner: &Arc<Inner>) -> Vec<crate::types::StylePackHotkey> {
    hotkey_runtime_target(inner)
        .style_packs
        .into_iter()
        .filter(|entry| {
            !is_unconfigured_shortcut(&entry.binding) && !is_modifier_only_shortcut(&entry.binding)
        })
        .collect()
}

/// 常驻 supervisor：持续比较 prefs 与实际注册表。状态一致时低频复查；配置变化、
/// 主动同步失败或只注册了部分条目时按 3s 节奏重试，直到收敛或 shutdown。
pub(super) fn style_pack_hotkey_supervisor_loop(inner: Arc<Inner>) {
    let mut attempts: u32 = 0;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }

        let desired = configured_style_pack_hotkeys(&inner);
        let registrations_match = {
            let registrations = inner.style_pack_hotkeys.lock();
            style_pack_hotkey_registrations_match(&desired, &registrations, |registration| {
                &registration.binding
            })
        };
        if registrations_match {
            attempts = 0;
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }

        match try_sync_style_pack_hotkeys_on_main_thread(&inner) {
            Ok(()) => {
                log::info!(
                    "[coord] style pack hotkey listeners synchronized after {} attempt(s)",
                    attempts + 1
                );
                attempts = 0;
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            Err(error) => {
                attempts += 1;
                if attempts <= 3 || attempts % 10 == 0 {
                    log::warn!(
                        "[coord] style pack hotkeys 第 {attempts} 次同步失败: {error}; 3s 后重试"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }
}

/// 按 runtime target 全量对齐风格包快捷键注册状态。**必须在主线程执行**（macOS Carbon
/// 要求 manager 在主线程构造）。策略为整表重建：先 drop 全部旧注册再逐条注册，
/// 避免「两个包互换按键」时新键仍被旧注册占用。任意条目失败会清空本轮全部注册。
pub(super) fn sync_style_pack_hotkeys(inner: &Arc<Inner>) -> Result<(), String> {
    let entries = configured_style_pack_hotkeys(inner);
    let mut registrations = inner.style_pack_hotkeys.lock();
    replace_style_pack_hotkey_registrations(&entries, &mut registrations, |entry| {
        let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
        let monitor = ComboHotkeyMonitor::start(entry.binding.clone(), tx).map_err(|error| {
            format!(
                "style pack hotkey {} registration failed: {error}",
                entry.pack_id
            )
        })?;
        let bridge_inner = Arc::clone(inner);
        let pack_id = entry.pack_id.clone();
        std::thread::Builder::new()
            .name("openless-style-pack-hotkey-bridge".into())
            .spawn(move || style_pack_hotkey_bridge_loop(bridge_inner, rx, pack_id))
            .map_err(|error| {
                format!(
                    "style pack hotkey {} bridge thread failed: {error}",
                    entry.pack_id
                )
            })?;
        Ok(StylePackHotkeyRegistration {
            binding: entry.binding.clone(),
            _monitor: monitor,
        })
    })
}

/// 事务式设置路径：派发到主线程并等待最多 5s，确保调用方能回滚偏好并展示错误。
pub(super) fn try_sync_style_pack_hotkeys_on_main_thread(inner: &Arc<Inner>) -> Result<(), String> {
    let (result_tx, result_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let sync_inner = Arc::clone(inner);
    inner.host.run_on_main_thread(move || {
        let _ = result_tx.send(sync_style_pack_hotkeys(&sync_inner));
    })?;
    result_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "注册风格包快捷键超时".to_string())?
}

/// 设置导入、删除风格包等不可整体回滚路径使用的主动同步。失败只记录日志，
/// 常驻 supervisor 会根据 prefs 与实际注册表差异继续重试。
pub(super) fn sync_style_pack_hotkeys_on_main_thread(inner: &Arc<Inner>) {
    let sync_inner = Arc::clone(inner);
    if let Err(error) = inner.host.run_on_main_thread(move || {
        if let Err(error) = sync_style_pack_hotkeys(&sync_inner) {
            log::warn!("[coord] style pack hotkeys 主动同步失败: {error}");
        }
    }) {
        log::warn!("[coord] dispatch style pack hotkeys sync failed: {error}");
    }
}

pub(super) fn clear_style_pack_hotkeys_on_main_thread(inner: &Arc<Inner>) {
    let main_inner = Arc::clone(inner);
    if inner
        .host
        .run_on_main_thread(move || {
            main_inner.style_pack_hotkeys.lock().clear();
        })
        .is_err()
    {
        inner.style_pack_hotkeys.lock().clear();
    }
}

pub(super) fn style_pack_hotkey_bridge_loop(
    inner: Arc<Inner>,
    rx: mpsc::Receiver<ComboHotkeyEvent>,
    pack_id: String,
) {
    while let Ok(evt) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            continue;
        }
        if matches!(evt, ComboHotkeyEvent::Pressed { .. }) {
            handle_style_pack_hotkey_pressed(&inner, &pack_id);
        }
    }
}

/// 复用 `activate_style_pack_by_id`（禁用包自动启用、写 prefs、sync、广播、刷托盘），
/// 与前端「点选风格包」走完全相同的激活路径；包已被删除时仅 warn 不做事。
pub(super) fn handle_style_pack_hotkey_pressed(inner: &Arc<Inner>, pack_id: &str) {
    let coord = Coordinator {
        inner: Arc::clone(inner),
    };
    match inner.host.activate_style_pack_by_id(&coord, pack_id) {
        Ok(pack) => {
            log::info!(
                "[coord] style pack hotkey activated {} ({})",
                pack.id,
                pack.name
            );
            #[cfg(not(mobile))]
            show_style_switch_capsule(inner, &pack.name);
        }
        Err(error) => {
            log::warn!("[coord] style pack hotkey {pack_id} activation failed: {error}")
        }
    }
}


/// Linux: 从 runtime target 读取自定义组合键，同步到 fcitx5 插件。
#[cfg(target_os = "linux")]
pub(super) fn custom_dictation_key_string(inner: &Arc<Inner>) -> Option<String> {
    let target = hotkey_runtime_target(inner);
    let key_string = crate::linux_fcitx::binding_to_fcitx_key_string(&target.dictation);
    if key_string.is_empty() {
        None
    } else {
        Some(key_string)
    }
}

#[cfg(target_os = "linux")]
pub(super) fn sync_custom_dictation_to_plugin(inner: &Arc<Inner>) {
    let target = hotkey_runtime_target(inner);
    let dictation = &target.dictation;
    let key_string = crate::linux_fcitx::binding_to_fcitx_key_string(dictation);
    if key_string.is_empty() {
        return;
    }
    match crate::linux_fcitx::set_custom_dictation_trigger(&key_string) {
        Ok(()) => log::info!(
            "[fcitx] Synced custom dictation trigger '{}' to plugin",
            key_string
        ),
        Err(e) => log::warn!("[fcitx] Failed to sync custom dictation trigger: {e}"),
    }
}

pub(super) fn modifier_shortcut_triggers(
    inner: &Arc<Inner>,
) -> (
    Option<crate::types::HotkeyTrigger>,
    Option<crate::types::HotkeyTrigger>,
    Option<crate::types::HotkeyTrigger>,
) {
    let target = hotkey_runtime_target(inner);
    // 纯本地:QA/翻译 modifier 触发已移除,只保留选区润色。
    let selection_polish_trigger = target
        .selection_polish
        .as_ref()
        .and_then(crate::shortcut_binding::legacy_modifier_trigger);
    (None, selection_polish_trigger, None)
}


pub(super) fn hotkey_bridge_loop(inner: Arc<Inner>, rx: mpsc::Receiver<HotkeyEvent>) {
    while let Ok(evt) = rx.recv() {
        if inner.shortcut_recording_active.load(Ordering::SeqCst) {
            // 录制态：仅上报「录制 Fn」事件给前端（recorder 在录入态检测到 Fn 按下，
            // 浏览器不向网页层下发 Fn keydown，由 CGEventTap 上报），其余热键事件
            // 一律跳过，避免录制期间误触发听写。
            #[cfg(not(mobile))]
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
            #[cfg(not(mobile))]
            HotkeyEvent::SelectionPolishShortcutPressed => {
                handle_selection_workspace_hotkey_pressed(&inner_cloned);
            }
            #[cfg(not(mobile))]
            HotkeyEvent::SelectionPolishShortcutReleased => {
                handle_selection_workspace_hotkey_released(&inner_cloned);
            }
            // 非录制态不会出现（CGEventTap 仅在 recording_active 时上报）；防御性忽略。
            #[cfg(not(mobile))]
            HotkeyEvent::FnRecordingPressed => {}
            // QA/翻译 modifier 触发已随云功能移除;监听器仍可能上报,防御性忽略。
            HotkeyEvent::TranslationModifierPressed | HotkeyEvent::QaShortcutPressed => {}
        }
    }
}

/// 录制态检测到 Fn 按下 → 发事件给前端，让 ShortcutRecorder 提交 Fn 绑定。
#[cfg(not(mobile))]
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
    if let Some(switch_style) = target.switch_style.as_ref() {
        if !is_modifier_only_shortcut(switch_style) {
            if let Some(monitor) = inner.switch_style_hotkey.lock().as_ref() {
                if let Err(e) = monitor.update_binding(switch_style.clone()) {
                    log::warn!("[coord] reset switch-style hotkey latch failed: {e}");
                }
            }
        }
    }
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

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (inner, event_type, key, code, repeat);
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        if !window_hotkey_fallback_enabled() {
            if event_type == "keydown" && !repeat {
                log::info!(
                    "[window-hotkey] ignored because Windows lifecycle owner is the low-level hook"
                );
            }
            return Ok(());
        }

        let Some(trigger) = crate::shortcut_binding::legacy_modifier_trigger(
            &hotkey_runtime_target(inner).dictation,
        ) else {
            return Ok(());
        };
        if !window_key_matches_trigger(trigger, &key, &code) {
            return Ok(());
        }

        match event_type.as_str() {
            "keydown" => {
                if repeat {
                    return Ok(());
                }
                log::info!(
                    "[window-hotkey] pressed trigger={trigger:?} code={code} repeat={repeat}"
                );
                let press_id = crate::hotkey::next_press_id();
                inner
                    .window_hotkey_press_id
                    .store(press_id, Ordering::SeqCst);
                handle_pressed_edge(inner, std::time::Instant::now(), press_id).await;
            }
            "keyup" => {
                log::info!("[window-hotkey] released trigger={trigger:?} code={code}");
                let press_id = inner.window_hotkey_press_id.swap(0, Ordering::SeqCst);
                handle_released_edge(inner, std::time::Instant::now(), press_id).await;
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn window_hotkey_fallback_enabled() -> bool {
    crate::types::HotkeyCapability::current().explicit_fallback_available
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn window_key_matches_trigger(
    trigger: crate::types::HotkeyTrigger,
    key: &str,
    code: &str,
) -> bool {
    use crate::types::HotkeyTrigger;

    match trigger {
        HotkeyTrigger::RightControl => key == "Control" && code == "ControlRight",
        HotkeyTrigger::LeftControl => key == "Control" && code == "ControlLeft",
        HotkeyTrigger::RightOption | HotkeyTrigger::RightAlt => {
            (key == "Alt" || key == "AltGraph") && code == "AltRight"
        }
        HotkeyTrigger::LeftOption => (key == "Alt" || key == "AltGraph") && code == "AltLeft",
        HotkeyTrigger::RightCommand => key == "Meta" && code == "MetaRight",
        HotkeyTrigger::LeftCommand => key == "Meta" && code == "MetaLeft",
        HotkeyTrigger::LeftShift => key == "Shift" && code == "ShiftLeft",
        HotkeyTrigger::RightShift => key == "Shift" && code == "ShiftRight",
        HotkeyTrigger::Fn => key == "Control" && code == "ControlRight",
        // MediaPlayPause 走 WH_KEYBOARD_LL，不走 window hotkey fallback
        HotkeyTrigger::MediaPlayPause => false,
        // Custom 走 global-hotkey crate，不走 window hotkey fallback
        HotkeyTrigger::Custom => false,
    }
}


#[cfg(any())]
mod tests {
    use super::*;

    fn style_hotkey(pack_id: &str, primary: &str) -> crate::types::StylePackHotkey {
        crate::types::StylePackHotkey {
            pack_id: pack_id.into(),
            binding: crate::types::ShortcutBinding {
                primary: primary.into(),
                modifiers: vec!["alt".into()],
            },
        }
    }

    #[test]
    fn style_pack_hotkey_registration_failure_leaves_no_partial_table() {
        let entries = [
            style_hotkey("builtin.raw", "1"),
            style_hotkey("imported.x", "2"),
        ];
        let mut registrations = std::collections::HashMap::from([("old".into(), 9_u8)]);

        let result =
            replace_style_pack_hotkey_registrations(&entries, &mut registrations, |entry| {
                if entry.pack_id == "imported.x" {
                    Err("register imported.x failed".into())
                } else {
                    Ok(1)
                }
            });

        assert_eq!(result.unwrap_err(), "register imported.x failed");
        assert!(registrations.is_empty());
    }

    #[test]
    fn style_pack_hotkey_registration_success_replaces_entire_table() {
        let entries = [
            style_hotkey("builtin.raw", "1"),
            style_hotkey("imported.x", "2"),
        ];
        let mut registrations = std::collections::HashMap::from([("old".into(), 9_u8)]);

        replace_style_pack_hotkey_registrations(&entries, &mut registrations, |entry| {
            Ok(if entry.pack_id == "builtin.raw" { 1 } else { 2 })
        })
        .unwrap();

        assert_eq!(registrations.len(), 2);
        assert_eq!(registrations.get("builtin.raw"), Some(&1));
        assert_eq!(registrations.get("imported.x"), Some(&2));
        assert!(!registrations.contains_key("old"));
    }

    #[test]
    fn style_pack_hotkey_registration_state_matches_exact_bindings() {
        let desired = [
            style_hotkey("builtin.raw", "1"),
            style_hotkey("imported.x", "2"),
        ];
        let registrations = desired
            .iter()
            .map(|entry| (entry.pack_id.clone(), entry.binding.clone()))
            .collect();

        assert!(style_pack_hotkey_registrations_match(
            &desired,
            &registrations,
            |binding| binding,
        ));
    }

    #[test]
    fn style_pack_hotkey_registration_state_detects_changed_or_missing_bindings() {
        let desired = [
            style_hotkey("builtin.raw", "1"),
            style_hotkey("imported.x", "2"),
        ];
        let changed = std::collections::HashMap::from([
            (
                "builtin.raw".into(),
                style_hotkey("builtin.raw", "9").binding,
            ),
            ("imported.x".into(), desired[1].binding.clone()),
        ]);
        let partial =
            std::collections::HashMap::from([("builtin.raw".into(), desired[0].binding.clone())]);

        assert!(!style_pack_hotkey_registrations_match(
            &desired,
            &changed,
            |binding| binding,
        ));
        assert!(!style_pack_hotkey_registrations_match(
            &desired,
            &partial,
            |binding| binding,
        ));
        assert!(style_pack_hotkey_registrations_match(
            &[],
            &std::collections::HashMap::<String, crate::types::ShortcutBinding>::new(),
            |binding| binding,
        ));
        assert!(!style_pack_hotkey_registrations_match(
            &[style_hotkey("builtin.raw", "1")],
            &std::collections::HashMap::<String, crate::types::ShortcutBinding>::new(),
            |binding| binding,
        ));
    }

    /// 轮询 `inner.state.cancelled` 直到满足条件，超时返回 false。
    fn wait_until(mut cond: impl FnMut() -> bool, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// 构造旧 Coordinator 状态，用注入的 handler 验证 bridge 自身的信号语义。
    fn coordinator_in_processing() -> Coordinator {
        let coordinator = Coordinator::new();
        let mut state = coordinator.inner.state.lock();
        state.phase = SessionPhase::Processing;
        state.cancelled = false;
        drop(state);
        coordinator
    }

    /// 后台运行 esc_cancel_bridge_loop，返回 sender 与 join handle。
    fn spawn_loop(inner: &Arc<Inner>) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel::<()>();
        let bridge_inner = Arc::clone(inner);
        let handle = std::thread::spawn(move || {
            esc_cancel_bridge_loop_with(bridge_inner, rx, |inner| {
                inner.state.lock().cancelled = true;
            })
        });
        (tx, handle)
    }

    #[test]
    fn esc_cancel_bridge_sets_cancelled_during_processing() {
        let coordinator = coordinator_in_processing();
        let (tx, handle) = spawn_loop(&coordinator.inner);

        tx.send(()).unwrap();
        assert!(
            wait_until(
                || coordinator.inner.state.lock().cancelled,
                std::time::Duration::from_secs(2)
            ),
            "取消信号应置 cancelled 旗标"
        );
        // #798 语义：Processing 阶段保持 phase=Processing，由 end_session 自行收尾。
        assert_eq!(
            coordinator.inner.state.lock().phase,
            SessionPhase::Processing
        );

        drop(tx);
        handle.join().unwrap();
    }

    #[test]
    fn esc_cancel_bridge_skips_while_shortcut_recording_active() {
        let coordinator = coordinator_in_processing();
        let (tx, handle) = spawn_loop(&coordinator.inner);

        coordinator.set_shortcut_recording_active(true);
        tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(
            !coordinator.inner.state.lock().cancelled,
            "录制快捷键期间按 Esc 应被忽略"
        );

        // 录制结束后 Esc 恢复生效。
        coordinator.set_shortcut_recording_active(false);
        tx.send(()).unwrap();
        assert!(
            wait_until(
                || coordinator.inner.state.lock().cancelled,
                std::time::Duration::from_secs(2)
            ),
            "录制结束后取消信号应恢复生效"
        );

        drop(tx);
        handle.join().unwrap();
    }

    #[test]
    fn esc_cancel_bridge_is_idempotent_on_repeat_signals() {
        let coordinator = coordinator_in_processing();
        let (tx, handle) = spawn_loop(&coordinator.inner);

        for _ in 0..3 {
            tx.send(()).unwrap();
        }
        assert!(
            wait_until(
                || coordinator.inner.state.lock().cancelled,
                std::time::Duration::from_secs(2)
            ),
            "首个取消信号应置 cancelled 旗标"
        );
        // 连按 Esc / 双通道重复触发时 cancel_session 幂等：不 panic、状态不回写。
        tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert_eq!(
            coordinator.inner.state.lock().phase,
            SessionPhase::Processing
        );

        drop(tx);
        handle.join().unwrap();
    }
}