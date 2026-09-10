//! 全局热键监听：发送按下 / 抬起 / 取消三类边沿事件。
//!
//! - macOS：原生 CGEventTap（core-foundation + core-graphics FFI），与 Swift
//!   `OpenLessHotkey/HotkeyMonitor.swift` 同源。
//! - Windows：原生 `WH_KEYBOARD_LL` low-level keyboard hook，保留 modifier-only
//!   trigger（如右 Control / 右 Alt）的真实语义。
//! - Linux：fcitx5 插件提供热键事件（DBus 信号 `DictationKeyEvent`）。
//!
//! 仅产出带代次和单调时间戳的原始边沿，业务语义由 openless-core 解释。
//!
//! Esc（取消）与组合键撤销（触发键按住期间叠加了普通键）**都不走** `HotkeyEvent`
//! 通道，而是独立的 `Sender<HotkeyCombinedEdge>`：Pressed/Released 的 bridge 线程为了修 #468/#475 的
//! latch 竞态改成了串行 block_on，Pressed / Released 会在 bridge 线程上同步跑完
//! `begin_session`（开麦 + ASR 握手）或整个转写 + 润色流程 —— 若取消 / 撤销与它们同
//! 队列，事件只能排队等流程跑完，观感就是「晚几百毫秒才生效」。独立通道 + 专用消费
//! 线程保证取消 / 撤销随到随处理（`cancel_session` / `handle_trigger_combined` 都是纯
//! 同步快路径：置旗标 + 清资源，不 await）。
use crate::types::{HotkeyAdapterKind, HotkeyBinding, HotkeyCapability, HotkeyInstallError};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed {
        at: Instant,
        press_id: u64,
    },
    Released {
        at: Instant,
        press_id: u64,
    },
    /// 录制态按下 Fn（浏览器不向网页层下发 Fn 的 keydown，无法通过 recorder 捕获；
    /// 由 CGEventTap 在录制态检测后上报，供前端 ShortcutRecorder 提交 Fn 绑定）。
    FnRecordingPressed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyCombinedEdge {
    pub at: Instant,
    pub press_id: u64,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HotkeyTrigger;
    use std::sync::atomic::Ordering;
    fn shared_with_held_latches() -> Shared {
        Shared {
            binding: RwLock::new(HotkeyBinding::default()),
            trigger_held: AtomicBool::new(true),
            trigger_press_id: AtomicU64::new(42),
            trigger_companion_seen: AtomicU64::new(42),
            recording_active: AtomicBool::new(false),
            recording_fn_held: AtomicBool::new(false),
        }
    }
    #[test]
    fn reset_shared_held_state_clears_all_shortcut_latches() {
        let shared = shared_with_held_latches();
        reset_shared_held_state(&shared);
        assert!(!shared.trigger_held.load(Ordering::SeqCst));
        assert_eq!(shared.trigger_press_id.load(Ordering::SeqCst), 0);
        assert_eq!(shared.trigger_companion_seen.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn update_binding_resets_only_dictation_latch() {
        let shared = shared_with_held_latches();
        let next = HotkeyBinding {
            trigger: HotkeyTrigger::LeftControl,
            mode: crate::types::HotkeyMode::Hold,
            keys: None,
        };
        update_shared_binding(&shared, next.clone());
        assert_eq!(*shared.binding.read(), next);
        assert!(!shared.trigger_held.load(Ordering::SeqCst));
        assert_eq!(shared.trigger_press_id.load(Ordering::SeqCst), 0);
        assert_eq!(shared.trigger_companion_seen.load(Ordering::SeqCst), 0);
    }
}
pub trait HotkeyAdapter: Send + Sync {
    fn kind(&self) -> HotkeyAdapterKind;
    fn update_binding(&self, binding: HotkeyBinding);
    fn reset_held_state(&self);
    /// 快捷键录制态开关。激活期间监听器上报 Fn 按下边沿（`FnRecordingPressed`），
    /// 供前端 recorder 提交 Fn 绑定（浏览器不向网页层下发 Fn keydown）。
    fn set_recording_active(&self, _active: bool) {}
    fn shutdown(&self) {}
}
struct Shared {
    binding: RwLock<HotkeyBinding>,
    /// 触发键当前是否处于"按住"状态。OS 自动重复事件用此去重。
    trigger_held: AtomicBool,
    /// 当前触发键按下的全局代次。代次由监听器生成，避免独立撤销通道的迟到事件
    /// 误认成下一次按下。
    trigger_press_id: AtomicU64,
    /// 已经看到普通键的按下代次；0 表示本次按下尚未看到普通键。
    trigger_companion_seen: AtomicU64,
    /// 快捷键录制是否激活（ShortcutRecorder 处于录入态）。激活期间 CGEventTap 上报
    /// Fn 按下边沿（`FnRecordingPressed`），供前端 recorder 提交 Fn 绑定。
    recording_active: AtomicBool,
    /// 录制态 Fn 是否已按住，用于在 FLAGS_CHANGED 上识别 down 边沿（去重）。
    recording_fn_held: AtomicBool,
}
pub struct HotkeyMonitor {
    adapter: Box<dyn HotkeyAdapter>,
}
impl HotkeyMonitor {
    /// Spawn the listener thread and **wait synchronously** for it to confirm
    /// the OS-level hook installed so the caller can surface an actual adapter
    /// status instead of silently dropping events.
    ///
    /// `cancel_tx`：Esc 按下即发一个 `()`。独立于 `tx`，见模块注释——不能与
    /// Pressed/Released 挤同一条串行 bridge，否则 Processing 期间取消排不上队。
    /// `combo_tx`：触发键按住期间叠加了普通键就发一个带时间戳的 press edge。独立于 `tx`，见模块
    /// 注释——不能与 Pressed/Released 挤同一条串行 bridge，否则撤销要等
    /// `begin_session` 开完麦才排得上队，胶囊晚几百毫秒才消失。
    pub fn start(
        binding: HotkeyBinding,
        tx: Sender<HotkeyEvent>,
        cancel_tx: Sender<()>,
        combo_tx: Sender<HotkeyCombinedEdge>,
    ) -> Result<Self, HotkeyInstallError> {
        Ok(Self {
            adapter: platform::start_adapter(binding, tx, cancel_tx, combo_tx)?,
        })
    }
    pub fn update_binding(&self, binding: HotkeyBinding) {
        self.adapter.update_binding(binding);
    }

    pub fn kind(&self) -> HotkeyAdapterKind {
        self.adapter.kind()
    }
    pub fn reset_held_state(&self) {
        self.adapter.reset_held_state();
    }
    pub fn set_recording_active(&self, active: bool) {
        self.adapter.set_recording_active(active);
    }
    pub fn capability() -> HotkeyCapability {
        HotkeyCapability::current()
    }
}
impl Drop for HotkeyMonitor {
    fn drop(&mut self) {
        self.adapter.shutdown();
    }
}
fn install_error(code: &str, message: impl Into<String>) -> HotkeyInstallError {
    HotkeyInstallError {
        code: code.into(),
        message: message.into(),
    }
}
fn send_or_log(tx: &Sender<HotkeyEvent>, evt: HotkeyEvent) {
    if let Err(e) = tx.send(evt) {
        log::warn!("[hotkey] 事件发送失败: {e}");
    }
}
fn send_cancel_or_log(tx: &Sender<()>) {
    if let Err(e) = tx.send(()) {
        log::warn!("[hotkey] 取消事件发送失败: {e}");
    }
}
fn send_combo_abort_or_log(tx: &Sender<HotkeyCombinedEdge>, press_id: u64) {
    if let Err(e) = tx.send(HotkeyCombinedEdge {
        at: Instant::now(),
        press_id,
    }) {
        log::warn!("[hotkey] 组合键撤销事件发送失败: {e}");
    }
}
static NEXT_PRESS_ID: AtomicU64 = AtomicU64::new(1);
pub(crate) fn next_press_id() -> u64 {
    NEXT_PRESS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
/// 会话激活期间（胶囊显示录音/转写/润色中）Esc 由 OpenLess 独占：tap/hook 吞掉
/// keydown 不透传宿主应用。否则一次 Esc 双重生效——既取消 OpenLess 会话，又触发
/// 宿主应用自己的 Esc 语义（如取消 Claude 正在生成的回复）。对照输入法的行为：
/// 组合窗激活时 Esc 只取消候选词、宿主应用收不到。keyup 不吞：宿主应用几乎都在
/// keydown 上响应 Esc，孤儿 keyup 无害，且窗口期内会话结束时吞 up 不吞 down 反而
/// 会造成不成对。由 coordinator 的 emit_capsule 在胶囊状态变化时更新。
static ESC_EXCLUSIVE: AtomicBool = AtomicBool::new(false);
pub fn set_esc_exclusive(active: bool) {
    ESC_EXCLUSIVE.store(active, std::sync::atomic::Ordering::SeqCst);
}
fn esc_exclusive() -> bool {
    ESC_EXCLUSIVE.load(std::sync::atomic::Ordering::SeqCst)
}
type StartupTx<T> = mpsc::Sender<Result<T, HotkeyInstallError>>;
struct ListenerThread<T> {
    shared: Arc<Shared>,
    startup: T,
}
fn start_listener_thread<T, F>(
    binding: HotkeyBinding,
    tx: Sender<HotkeyEvent>,
    cancel_tx: Sender<()>,
    combo_tx: Sender<HotkeyCombinedEdge>,
    thread_name: &str,
    startup_timeout_message: &'static str,
    run_listen_loop: F,
) -> Result<ListenerThread<T>, HotkeyInstallError>
where
    T: Send + 'static,
    F: FnOnce(
            Arc<Shared>,
            Sender<HotkeyEvent>,
            Sender<()>,
            Sender<HotkeyCombinedEdge>,
            StartupTx<T>,
        ) + Send
        + 'static,
{
    let shared = Arc::new(Shared {
        binding: RwLock::new(binding),
        trigger_held: AtomicBool::new(false),
        trigger_press_id: AtomicU64::new(0),
        trigger_companion_seen: AtomicU64::new(0),
        recording_active: AtomicBool::new(false),
        recording_fn_held: AtomicBool::new(false),
    });
    let thread_shared = Arc::clone(&shared);
    let (status_tx, status_rx) = mpsc::channel::<Result<T, HotkeyInstallError>>();
    std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || run_listen_loop(thread_shared, tx, cancel_tx, combo_tx, status_tx))
        .map_err(|e| install_error("spawn_failed", format!("hotkey 线程启动失败: {e}")))?;
    match status_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(startup)) => Ok(ListenerThread { shared, startup }),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(install_error("startup_timeout", startup_timeout_message)),
    }
}
fn update_shared_binding(shared: &Shared, binding: HotkeyBinding) {
    {
        let mut current = shared.binding.write();
        if *current == binding {
            return;
        }
        *current = binding;
    }
    shared
        .trigger_held
        .store(false, std::sync::atomic::Ordering::SeqCst);
    shared
        .trigger_press_id
        .store(0, std::sync::atomic::Ordering::SeqCst);
    shared
        .trigger_companion_seen
        .store(0, std::sync::atomic::Ordering::SeqCst);
}

fn reset_shared_held_state(shared: &Shared) {
    shared
        .trigger_held
        .store(false, std::sync::atomic::Ordering::SeqCst);
    shared
        .trigger_companion_seen
        .store(0, std::sync::atomic::Ordering::SeqCst);
    shared
        .trigger_press_id
        .store(0, std::sync::atomic::Ordering::SeqCst);
}
mod platform {
    use super::{
        esc_exclusive, install_error, reset_shared_held_state, send_cancel_or_log,
        send_combo_abort_or_log, send_or_log, start_listener_thread, update_shared_binding,
        HotkeyAdapter, HotkeyCombinedEdge, HotkeyEvent, Shared, StartupTx,
    };
    use crate::types::{HotkeyAdapterKind, HotkeyBinding, HotkeyInstallError, HotkeyTrigger};
    use std::ffi::c_void;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    pub fn start_adapter(
        binding: HotkeyBinding,
        tx: Sender<HotkeyEvent>,
        cancel_tx: Sender<()>,
        combo_tx: Sender<HotkeyCombinedEdge>,
    ) -> Result<Box<dyn HotkeyAdapter>, HotkeyInstallError> {
        let listener = start_listener_thread(
            binding,
            tx,
            cancel_tx,
            combo_tx,
            "openless-hotkey-mac-event-tap",
            "hotkey hook 启动超时",
            run_listen_loop,
        )?;
        Ok(Box::new(MacHotkeyAdapter {
            shared: listener.shared,
            handles: listener.startup,
        }))
    }
    /// Refs needed to stop the Mac CFRunLoop / CGEventTap from outside the listener
    /// thread. Filled in by `run_listen_loop` once the tap is created and the runloop
    /// reference is captured; consumed by `MacHotkeyAdapter::shutdown` when the
    /// monitor is dropped (so a binding swap or app shutdown doesn't leak the
    /// listener thread + tap). Cf. audit 3.1.1.
    struct MacShutdownHandles {
        tap: std::sync::Mutex<Option<CfMachPortRef>>,
        runloop: std::sync::Mutex<Option<CfRunLoopRef>>,
    }
    unsafe impl Send for MacShutdownHandles {}
    unsafe impl Sync for MacShutdownHandles {}
    struct MacHotkeyAdapter {
        shared: Arc<Shared>,
        handles: Arc<MacShutdownHandles>,
    }
    impl HotkeyAdapter for MacHotkeyAdapter {
        fn kind(&self) -> HotkeyAdapterKind {
            HotkeyAdapterKind::MacEventTap
        }
        fn update_binding(&self, binding: HotkeyBinding) {
            update_shared_binding(&self.shared, binding);
        }
        fn reset_held_state(&self) {
            reset_shared_held_state(&self.shared);
        }
        fn set_recording_active(&self, active: bool) {
            self.shared.recording_active.store(active, Ordering::SeqCst);
            if !active {
                self.shared.recording_fn_held.store(false, Ordering::SeqCst);
            }
        }
        fn shutdown(&self) {
            let tap = self.handles.tap.lock().ok().and_then(|mut g| g.take());
            if let Some(tap) = tap {
                unsafe { CGEventTapEnable(tap, false) };
            }
            let runloop = self.handles.runloop.lock().ok().and_then(|mut g| g.take());
            if let Some(rl) = runloop {
                unsafe { CFRunLoopStop(rl) };
            }
        }
    }
    #[repr(C)]
    struct OpaqueCgEvent(c_void);
    type CgEventRef = *mut OpaqueCgEvent;
    #[repr(C)]
    struct OpaqueCfMachPort(c_void);
    type CfMachPortRef = *mut OpaqueCfMachPort;
    #[repr(C)]
    struct OpaqueCfRunLoop(c_void);
    type CfRunLoopRef = *mut OpaqueCfRunLoop;
    #[repr(C)]
    struct OpaqueCfRunLoopSource(c_void);
    type CfRunLoopSourceRef = *mut OpaqueCfRunLoopSource;
    type CfStringRef = *const c_void;
    type CfAllocatorRef = *const c_void;
    type CgEventMask = u64;
    type CgEventType = u32;
    type CgEventTapLocation = u32;
    type CgEventTapPlacement = u32;
    type CgEventTapOptions = u32;
    type CgEventField = u32;
    type CgEventFlags = u64;
    const SESSION_EVENT_TAP: CgEventTapLocation = 1;
    const HEAD_INSERT: CgEventTapPlacement = 0;
    const TAP_OPTION_DEFAULT: CgEventTapOptions = 0;
    const KEY_DOWN: CgEventType = 10;
    const KEY_UP: CgEventType = 11;
    const FLAGS_CHANGED: CgEventType = 12;
    const TAP_DISABLED_BY_TIMEOUT: CgEventType = 0xFFFF_FFFE;
    const TAP_DISABLED_BY_USER_INPUT: CgEventType = 0xFFFF_FFFF;
    const KEYBOARD_EVENT_KEYCODE: CgEventField = 9;
    const FLAG_MASK_SHIFT: CgEventFlags = 0x0002_0000;
    const FLAG_MASK_CONTROL: CgEventFlags = 0x0004_0000;
    const FLAG_MASK_ALTERNATE: CgEventFlags = 0x0008_0000;
    const FLAG_MASK_COMMAND: CgEventFlags = 0x0010_0000;
    const FLAG_MASK_SECONDARY_FN: CgEventFlags = 0x0080_0000;
    const ESC_KEYCODE: i64 = 53;
    type CgEventTapCallBack = extern "C" fn(
        proxy: *mut c_void,
        event_type: CgEventType,
        event: CgEventRef,
        user_info: *mut c_void,
    ) -> CgEventRef;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: CgEventTapLocation,
            place: CgEventTapPlacement,
            options: CgEventTapOptions,
            events_of_interest: CgEventMask,
            callback: CgEventTapCallBack,
            user_info: *mut c_void,
        ) -> CfMachPortRef;
        fn CGEventTapEnable(tap: CfMachPortRef, enable: bool);
        fn CGEventGetIntegerValueField(event: CgEventRef, field: CgEventField) -> i64;
        fn CGEventGetFlags(event: CgEventRef) -> CgEventFlags;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: CfAllocatorRef,
            port: CfMachPortRef,
            order: isize,
        ) -> CfRunLoopSourceRef;
        fn CFRunLoopGetCurrent() -> CfRunLoopRef;
        fn CFRunLoopAddSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: CfRunLoopRef);
        static kCFRunLoopCommonModes: CfStringRef;
    }
    struct CallbackContext {
        shared: Arc<Shared>,
        tx: Sender<HotkeyEvent>,
        /// Esc 专用通道，见模块注释——不与 tx 挤同一条串行 bridge。
        cancel_tx: Sender<()>,
        /// 组合键撤销专用通道，见模块注释——不与 tx 挤同一条串行 bridge。
        combo_tx: Sender<HotkeyCombinedEdge>,
        /// 与 MacHotkeyAdapter 共享的 (tap, runloop) refs。tap re-enable on
        /// TAP_DISABLED_BY_TIMEOUT 走 handles.tap；adapter shutdown 也走这两个 lock。
        handles: Arc<MacShutdownHandles>,
    }
    unsafe impl Send for CallbackContext {}
    unsafe impl Sync for CallbackContext {}
    fn run_listen_loop(
        shared: Arc<Shared>,
        tx: Sender<HotkeyEvent>,
        cancel_tx: Sender<()>,
        combo_tx: Sender<HotkeyCombinedEdge>,
        status_tx: StartupTx<Arc<MacShutdownHandles>>,
    ) {
        let mask: CgEventMask = (1u64 << FLAGS_CHANGED) | (1u64 << KEY_DOWN) | (1u64 << KEY_UP);
        let handles = Arc::new(MacShutdownHandles {
            tap: std::sync::Mutex::new(None),
            runloop: std::sync::Mutex::new(None),
        });
        let context = Box::into_raw(Box::new(CallbackContext {
            shared,
            tx,
            cancel_tx,
            combo_tx,
            handles: Arc::clone(&handles),
        }));
        unsafe {
            let tap = CGEventTapCreate(
                SESSION_EVENT_TAP,
                HEAD_INSERT,
                TAP_OPTION_DEFAULT,
                mask,
                tap_callback,
                context as *mut c_void,
            );
            if tap.is_null() {
                log::warn!(
                    "[hotkey] CGEventTapCreate 失败 — Accessibility 权限未授予。Coordinator 会重试。"
                );
                let _ = Box::from_raw(context);
                let _ = status_tx.send(Err(install_error(
                    "accessibility_denied",
                    "hotkey hook 安装失败（辅助功能权限未授予）",
                )));
                return;
            }
            *handles.tap.lock().unwrap() = Some(tap);
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            let runloop = CFRunLoopGetCurrent();
            *handles.runloop.lock().unwrap() = Some(runloop);
            CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            log::info!("[hotkey] CGEventTap 已启动");
            let _ = status_tx.send(Ok(handles));
            CFRunLoopRun();
            let _ = Box::from_raw(context);
        }
    }
    extern "C" fn tap_callback(
        _proxy: *mut c_void,
        event_type: CgEventType,
        event: CgEventRef,
        user_info: *mut c_void,
    ) -> CgEventRef {
        if user_info.is_null() {
            return event;
        }
        let ctx = unsafe { &*(user_info as *const CallbackContext) };
        match event_type {
            TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
                if let Some(tap) = *ctx.handles.tap.lock().unwrap() {
                    unsafe { CGEventTapEnable(tap, true) };
                }
                return event;
            }
            FLAGS_CHANGED => handle_flags_changed(ctx, event),
            KEY_DOWN => {
                handle_key_down(ctx, event);
                let keycode = unsafe { CGEventGetIntegerValueField(event, KEYBOARD_EVENT_KEYCODE) };
                crate::side_aware_combo::platform::dispatch_keycode(keycode, false, 0, true);
                if keycode == ESC_KEYCODE && esc_exclusive() {
                    return std::ptr::null_mut();
                }
            }
            KEY_UP => {
                let keycode = unsafe { CGEventGetIntegerValueField(event, KEYBOARD_EVENT_KEYCODE) };
                crate::side_aware_combo::platform::dispatch_keycode(keycode, false, 0, false);
            }
            _ => {}
        }
        event
    }
    fn handle_flags_changed(ctx: &CallbackContext, event: CgEventRef) {
        let flags = unsafe { CGEventGetFlags(event) };
        if ctx.shared.recording_active.load(Ordering::SeqCst) {
            let fn_active = (flags & FLAG_MASK_SECONDARY_FN) != 0;
            let fn_was_held = ctx.shared.recording_fn_held.load(Ordering::SeqCst);
            if fn_active && !fn_was_held {
                ctx.shared.recording_fn_held.store(true, Ordering::SeqCst);
                log::info!("[hotkey] 录制态检测到 Fn↓ → 上报 FnRecordingPressed");
                send_or_log(&ctx.tx, HotkeyEvent::FnRecordingPressed);
            } else if !fn_active && fn_was_held {
                ctx.shared.recording_fn_held.store(false, Ordering::SeqCst);
                log::info!("[hotkey] 录制态 Fn↑（松开）");
            }
        }
        let keycode = unsafe { CGEventGetIntegerValueField(event, KEYBOARD_EVENT_KEYCODE) };
        crate::side_aware_combo::platform::dispatch_keycode(keycode, true, flags, false);
        let trigger = ctx.shared.binding.read().trigger;
        if trigger == HotkeyTrigger::Custom {
            return;
        }
        let expected_keycode = trigger_to_keycode(trigger);
        if keycode != expected_keycode {
            return;
        }
        let mask = trigger_to_flag_mask(trigger);
        let is_active = (flags & mask) != 0;
        let was_held = ctx.shared.trigger_held.load(Ordering::SeqCst);
        if is_active && !was_held {
            ctx.shared.trigger_held.store(true, Ordering::SeqCst);
            let press_id = super::next_press_id();
            ctx.shared
                .trigger_press_id
                .store(press_id, Ordering::SeqCst);
            ctx.shared.trigger_companion_seen.store(0, Ordering::SeqCst);
            send_or_log(
                &ctx.tx,
                HotkeyEvent::Pressed {
                    at: std::time::Instant::now(),
                    press_id,
                },
            );
        } else if !is_active && was_held {
            ctx.shared.trigger_held.store(false, Ordering::SeqCst);
            let press_id = ctx.shared.trigger_press_id.swap(0, Ordering::SeqCst);
            send_or_log(
                &ctx.tx,
                HotkeyEvent::Released {
                    at: std::time::Instant::now(),
                    press_id,
                },
            );
        }
    }
    fn handle_key_down(ctx: &CallbackContext, event: CgEventRef) {
        let keycode = unsafe { CGEventGetIntegerValueField(event, KEYBOARD_EVENT_KEYCODE) };
        if keycode == ESC_KEYCODE {
            note_companion_key_down(ctx);
            send_cancel_or_log(&ctx.cancel_tx);
            return;
        }
        note_companion_key_down(ctx);
    }
    /// 触发键按住期间按下任意普通键 = 用户在打组合键（Option+任意字母/数字键、Option+Tab…），
    /// 不是想说话 —— 往组合键撤销通道发一次让上层撤销这次按下。
    ///
    /// 走 `combo_tx` 而不是 `tx`：撤销必须在按下 Q 的那一帧就生效（胶囊立刻消失），
    /// 而 `tx` 那头的 bridge 此刻多半正卡在这次按下自己的 `begin_session` 里。见模块注释。
    ///
    /// macOS 的修饰键走 FLAGS_CHANGED、不会进 KEY_DOWN，所以叠加 Shift（翻译修饰键）
    /// 或 Cmd 不会被误判成组合键；只有真正的字符 / 功能键才算。OS 自动重复与「按住
    /// 触发键连按多个键」由 companion latch 收敛成一次。
    fn note_companion_key_down(ctx: &CallbackContext) {
        if !ctx.shared.trigger_held.load(Ordering::SeqCst) {
            return;
        }
        let press_id = ctx.shared.trigger_press_id.load(Ordering::SeqCst);
        if press_id == 0
            || ctx
                .shared
                .trigger_companion_seen
                .compare_exchange(0, press_id, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        log::info!("[hotkey] 触发键与其他键组合按下 —— 撤销本次触发");
        send_combo_abort_or_log(&ctx.combo_tx, press_id);
    }
    fn trigger_to_keycode(trigger: HotkeyTrigger) -> i64 {
        match trigger {
            HotkeyTrigger::LeftControl => 59,
            HotkeyTrigger::RightControl => 62,
            HotkeyTrigger::LeftOption => 58,
            HotkeyTrigger::RightOption | HotkeyTrigger::RightAlt => 61,
            HotkeyTrigger::RightCommand => 54,
            HotkeyTrigger::LeftCommand => 55,
            HotkeyTrigger::LeftShift => 56,
            HotkeyTrigger::RightShift => 60,
            HotkeyTrigger::Fn => 63,
            HotkeyTrigger::MediaPlayPause => 0,
            HotkeyTrigger::Custom => {
                unreachable!("custom combo hotkeys use ComboHotkeyMonitor")
            }
        }
    }
    fn trigger_to_flag_mask(trigger: HotkeyTrigger) -> CgEventFlags {
        match trigger {
            HotkeyTrigger::LeftControl | HotkeyTrigger::RightControl => FLAG_MASK_CONTROL,
            HotkeyTrigger::LeftCommand | HotkeyTrigger::RightCommand => FLAG_MASK_COMMAND,
            HotkeyTrigger::LeftShift | HotkeyTrigger::RightShift => FLAG_MASK_SHIFT,
            HotkeyTrigger::LeftOption | HotkeyTrigger::RightOption | HotkeyTrigger::RightAlt => {
                FLAG_MASK_ALTERNATE
            }
            HotkeyTrigger::Fn => FLAG_MASK_SECONDARY_FN,
            HotkeyTrigger::MediaPlayPause => 0,
            HotkeyTrigger::Custom => {
                unreachable!("custom combo hotkeys use ComboHotkeyMonitor")
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use parking_lot::RwLock;
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::mpsc;
        fn shared(trigger: HotkeyTrigger) -> Arc<Shared> {
            Arc::new(Shared {
                binding: RwLock::new(HotkeyBinding {
                    trigger,
                    mode: crate::types::HotkeyMode::Toggle,
                    keys: None,
                }),
                trigger_held: AtomicBool::new(false),
                trigger_press_id: AtomicU64::new(0),
                trigger_companion_seen: AtomicU64::new(0),
                recording_active: AtomicBool::new(false),
                recording_fn_held: AtomicBool::new(false),
            })
        }
        fn callback_context_with_combo(
            shared: Arc<Shared>,
        ) -> (
            CallbackContext,
            mpsc::Receiver<HotkeyEvent>,
            mpsc::Receiver<HotkeyCombinedEdge>,
        ) {
            let (tx, rx) = mpsc::channel();
            let (cancel_tx, _cancel_rx) = mpsc::channel();
            let (combo_tx, combo_rx) = mpsc::channel();
            (
                CallbackContext {
                    shared,
                    tx,
                    cancel_tx,
                    combo_tx,
                    handles: Arc::new(MacShutdownHandles {
                        tap: std::sync::Mutex::new(None),
                        runloop: std::sync::Mutex::new(None),
                    }),
                },
                rx,
                combo_rx,
            )
        }
        fn drain_combo(rx: &mpsc::Receiver<HotkeyCombinedEdge>) -> usize {
            rx.try_iter().count()
        }
        fn drain(rx: &mpsc::Receiver<HotkeyEvent>) -> Vec<HotkeyEvent> {
            rx.try_iter().collect()
        }

        #[test]
        fn mac_companion_key_down_aborts_trigger_once_per_hold() {
            let shared = shared(HotkeyTrigger::LeftOption);
            let (ctx, rx, combo_rx) = callback_context_with_combo(Arc::clone(&shared));
            note_companion_key_down(&ctx);
            assert_eq!(drain_combo(&combo_rx), 0);
            shared.trigger_press_id.store(1, Ordering::SeqCst);
            shared.trigger_held.store(true, Ordering::SeqCst);
            note_companion_key_down(&ctx);
            note_companion_key_down(&ctx);
            assert_eq!(drain_combo(&combo_rx), 1);
            shared.trigger_companion_seen.store(0, Ordering::SeqCst);
            note_companion_key_down(&ctx);
            assert_eq!(drain_combo(&combo_rx), 1);
            assert!(drain(&rx).is_empty());
        }
    }
}
