//! Hotkey supervisor / bridge loops and shortcut wiring extracted from
//! `coordinator.rs` (behavior-preserving move; see git history).
//!
//! Functions operate on the parent `Inner`/`Coordinator` and reference
//! parent-module items via `use super::*;`. Visibility is `pub(super)` so the
//! parent `coordinator` module can call them through `use hotkey_loops::*;`.

use super::*;

//// ─────────────────────────── hotkey bridging// ───────────────────────────

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
        // 组合键 = 用户不想听写：立刻停掉本次按下预置的麦克风流
        // （若已被正式会话接管则 slot 已空，cancel 无操作）。
        inner.prearm.cancel();
        handler(&inner, edge);
    }
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
            keys: None,
        };
        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let combo_tx = spawn_combo_abort_bridge(&inner, handle_trigger_combined);
        match HotkeyMonitor::start(binding, tx, combo_tx) {
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
                prearm_on_dictation_press(&inner_cloned);
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

pub(super) fn hotkey_runtime_target(inner: &Arc<Inner>) -> openless_core::HotkeyRuntimeTarget {
    inner.hotkey_runtime_target.lock().clone()
}

pub(super) fn is_unconfigured_shortcut(binding: &crate::types::ShortcutBinding) -> bool {
    binding.primary.trim().is_empty()
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
                prearm_on_dictation_press(&inner_cloned);
                inner.host.block_on(async {
                    handle_pressed_edge(&inner_cloned, at, press_id).await;
                });
            }
            HotkeyEvent::Released { at, press_id } => {
                inner.host.block_on(async {
                    handle_released_edge(&inner_cloned, at, press_id).await;
                });
            }
            // 组合键撤销不在此枚举里：走 combo_abort_bridge_loop，避免被上面
            // Released → end_session / Pressed → begin_session 的同步流程堵在
            // 队列里（见函数注释）。
            // 非录制态不会出现（CGEventTap 仅在 recording_active 时上报）；防御性忽略。
            HotkeyEvent::FnRecordingPressed => {}
        }
    }
}

/// 录制态检测到 Fn 按下 → 发事件给前端，让 ShortcutRecorder 提交 Fn 绑定。
///
/// 调用方是 `openless-hotkey-bridge` 工作线程（`hotkey_bridge_loop`）。
/// `emit_fn_shortcut_pressed` 内部经 `self.app()` 克隆 `AppHandle`，在工作线程调用会与
/// event-loop 线程竞争非原子引用计数；整段交给 `host.on_main_thread` 投递，克隆、发送
/// 与释放都发生在主线程。
fn emit_fn_recording_pressed(inner: &Arc<Inner>) {
    let inner_for_main = Arc::clone(inner);
    inner.host.on_main_thread(move || {
        log::info!(
            "[hotkey] 录制 Fn 按下 → emit fn-shortcut-pressed (app_ready={})",
            inner_for_main.host.is_bound()
        );
        if inner_for_main.host.is_bound() {
            inner_for_main.host.emit_fn_shortcut_pressed();
        }
    });
}

/// 听写键按下的瞬间预置麦克风流（详见 prearm 模块文档）。
///
/// 只在「这次按下会启动新会话」时预置：正在录音/转写时按下是停止语义，
/// 预置一条流只会白白点亮麦克风指示灯。会话状态判空用 core 快照；
/// 组合键撤销由 combo_abort_bridge_loop 里的 cancel 兜底。
fn prearm_on_dictation_press(inner: &Arc<Inner>) {
    if !crate::prearm::should_prearm(&inner.backend.snapshot()) {
        return;
    }
    let mic = crate::prearm::normalize_mic_name(
        &inner.backend.get_preferences().microphone_device_name,
    );
    inner.prearm.trigger(mic);
}

pub(super) fn reset_shortcut_held_state(inner: &Arc<Inner>) {
    if let Some(monitor) = inner.hotkey.lock().as_ref() {
        monitor.reset_held_state();
    }
}
