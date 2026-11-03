//! Dictation coordinator.
//!
//! Mirrors the Swift `DictationCoordinator` state machine. Single owner of
//! session state. Receives hotkey edges, drives recorder + ASR + polish +
//! insertion, persists history, emits `capsule:state` events to the capsule
//! window.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use crate::combo_hotkey::{ComboHotkeyError, ComboHotkeyEvent, ComboHotkeyMonitor};
use crate::hotkey::{HotkeyEvent, HotkeyMonitor};
use crate::persistence::{ActivityStore, DictionaryStore, HistoryStore, PreferencesStore};

use crate::types::{
    CapsulePayload, CapsuleState, CapsuleStyle, HotkeyCapability, HotkeyStatus, HotkeyStatusState,
};

mod capsule_focus;
#[path = "coordinator/dictation_core.rs"]
mod dictation;
mod hotkey_loops;
pub(crate) use capsule_focus::capture_frontmost_app;
use capsule_focus::*;
use hotkey_loops::*;

use dictation::{handle_pressed_edge, handle_released_edge, handle_trigger_combined};

/// 兜底卡片的窗口宽度（逻辑点）。比词条卡片宽一点 —— 这张要放一整段话。
const FALLBACK_CARD_WIDTH: f64 = 360.0;
/// Webview 首次渲染前的安全高度。真实高度由卡片 DOM 测量后通过 IPC 回报。
const FALLBACK_CARD_INITIAL_HEIGHT: f64 = 260.0;
/// 尺寸 IPC 的原生安全边界，不表达任何 CSS 布局规则。
const FALLBACK_CARD_MIN_HEIGHT: f64 = 96.0;
const FALLBACK_CARD_MAX_HEIGHT: f64 = 320.0;

fn validated_fallback_card_height(
    active_presentation_id: Option<u64>,
    presentation_id: u64,
    height: f64,
) -> Result<Option<f64>, String> {
    if !height.is_finite() {
        return Err("fallback card height must be finite".into());
    }
    if active_presentation_id != Some(presentation_id) {
        return Ok(None);
    }
    Ok(Some(
        height
            .ceil()
            .clamp(FALLBACK_CARD_MIN_HEIGHT, FALLBACK_CARD_MAX_HEIGHT),
    ))
}

/// 文本没能落到目标 app 时，把它连同一个复制按钮弹出来。
///
/// 为什么需要这张卡片：这些场景下唯一的兜底是「把文本写进剪贴板」，而它既依赖一个
/// 默认可关的开关，用户也**根本不知道文本在剪贴板里** —— 没有任何提示。屏幕上要么
/// 什么都没有，要么只有半截。
///
/// Window presentation uses the shared capsule behavior.
/// 右下角定位），理由见那里。多的一件事是 `insert_fallback_card_visible`：这张卡片
/// 在会话收尾那一刻弹出，而收尾自己安排了一次 `schedule_capsule_idle` → `hide()`，
/// 必须让那次 hide 认得出卡片并让路。
fn show_insert_fallback_card(inner: &Arc<Inner>, text: String, reason: &'static str) {
    if text.trim().is_empty() {
        return;
    }
    // 调用方是 tokio 工作线程（`forward_legacy_event` 处理 `InsertFallback`）。下面整段
    // 都要碰胶囊窗口句柄，因此整段交给 event-loop 线程执行：宿主槽位里的 `AppHandle`
    // 只借用不克隆，`capsule_window()` 也在主线程上取得。投递仍是一次事件循环任务，
    // 与收尾安排的延迟隐藏（`present_core_capsule_if_current` 里的定时任务）之间的先后
    // 不变 —— 卡片先登记自己占用窗口，后到的隐藏任务会让路。
    let inner_for_main = Arc::clone(inner);
    inner.host.on_main_thread(move || {
        let inner = inner_for_main;
        let Some(capsule) = inner.host.capsule_window() else {
            return;
        };
        // 与词条卡片同一道闸、同一理由：听写不在 Idle 就绝不碰这个窗口，否则等于把
        // 正在进行的那次听写的胶囊弄没了。收尾路径是先把 phase 置回 Idle 再走到这里的。
        if inner.backend.snapshot().dictation.phase != openless_core::DictationPhase::Idle {
            log::debug!("[fallback-card] suppressed: a dictation session is in flight");
            return;
        }
        let presentation_id = inner.host.begin_insert_fallback_card();
        let payload = crate::types::InsertFallbackCardPayload {
            text,
            reason: reason.to_string(),
            presentation_id,
        };
        if let Err(e) = capsule.set_cursor_passthrough(false) {
            log::warn!("[fallback-card] set_ignore_cursor_events(false) failed: {e}");
        }
        if let Err(e) = capsule.set_size(FALLBACK_CARD_WIDTH, FALLBACK_CARD_INITIAL_HEIGHT) {
            log::warn!("[fallback-card] resize failed: {e}");
        }
        if let Err(e) =
            capsule.position_fallback_card(FALLBACK_CARD_WIDTH, FALLBACK_CARD_INITIAL_HEIGHT)
        {
            log::warn!("[fallback-card] position failed: {e}");
        }
        // 位置同理：`maybe_position_capsule_bottom_center` 的去重缓存只记「显示器 +
        // 翻译态」，卡片这一挪它一无所知。不清掉的话下一次录音会判定「没变化」→
        // 跳过重新定位 → 胶囊留在卡片挪过去的右下角。
        capsule.invalidate_layout();
        inner.host.emit_insert_fallback(&payload);
        capsule.show_for_recording(true);
        #[cfg(target_os = "macos")]
        capsule.restore_main_window_key_if_active();
        log::info!(
            "[fallback-card] shown: reason={reason} chars={}",
            payload.text.chars().count()
        );
    });
}

fn report_insert_fallback_card_height(
    inner: &Arc<Inner>,
    presentation_id: u64,
    height: f64,
) -> Result<(), String> {
    // 调用方是 `#[tauri::command]`（不在 event-loop 线程）。活跃 presentation 的读取是
    // 纯 Mutex/Atomic 状态，留在调用线程；尺寸校验也在调用线程完成，命令的 Result 语义
    // （非法高度报错、过期 presentation 静默忽略）不因投递改变。
    let active_presentation_id = inner.host.active_insert_fallback_presentation_id();
    let Some(height) =
        validated_fallback_card_height(active_presentation_id, presentation_id, height)?
    else {
        return Ok(());
    };
    // 胶囊句柄只在 event-loop 线程上取得：宿主槽位里的 `AppHandle` 只借用不克隆。
    // 用宿主级 `run_on_main_thread`（Result 版）投递，未绑定或投递失败原样返回给命令。
    let inner_for_main = Arc::clone(inner);
    inner.host.run_on_main_thread(move || {
        let Some(capsule) = inner_for_main.host.capsule_window() else {
            return;
        };
        if !inner_for_main
            .host
            .insert_fallback_presentation_is_current(presentation_id)
        {
            return;
        }
        if let Err(e) = capsule.set_size(FALLBACK_CARD_WIDTH, height) {
            log::warn!("[fallback-card] measured resize failed: {e}");
        }
        if let Err(e) = capsule.position_fallback_card(FALLBACK_CARD_WIDTH, height) {
            log::warn!("[fallback-card] measured position failed: {e}");
        }
    })
}

/// 收起兜底卡片：把窗口完整还给胶囊。
///
/// Without an active card, return without changing the capsule so each dictation
/// 开始都会去 hide 那个窗口，和 `emit_capsule` 的 show 抢。
fn hide_insert_fallback_card(inner: &Arc<Inner>) {
    let _event_guard = inner.capsule_event_lock.lock();
    let (was_visible, deferred_capsule) = inner.host.dismiss_insert_fallback_card();
    if !was_visible {
        return;
    }
    // 状态读取（是否占用窗口、被推迟的 payload）是纯 Mutex/Atomic，留在调用线程即可；
    // 下面整段都要碰胶囊窗口句柄，而调用方是 `#[tauri::command]`（不在 event-loop
    // 线程），因此投递给宿主执行。`capsule_event_lock` 在投递前已取得：命令线程上
    // 投递只是立即返回的 post（主线程调用则同步执行），两种情形都不跨锁；
    // 闭包内不再碰这把锁。
    let inner_for_main = Arc::clone(inner);
    let _ = inner.host.run_on_main_thread(move || {
        let inner = inner_for_main;
        let Some(capsule) = inner.host.capsule_window() else {
            return;
        };
        inner.host.clear_insert_fallback();
        // 先隐藏再改几何：复原要同时动尺寸和位置，窗口还亮着时改就有概率被合成出
        // 一帧「卡片被拉宽、还横着飞过半个屏幕」。
        let _ = capsule.hide();
        // 穿透必须还回去，否则胶囊会一直挡着屏幕那一块。
        if let Err(e) = capsule.set_cursor_passthrough(true) {
            log::warn!("[fallback-card] restoring cursor passthrough failed: {e}");
        }
        // 尺寸也必须还回去 —— 卡片把窗口缩到过自己的大小，不复原的话下一次胶囊
        // 就挤在一个卡片大小的窗口里，等于看不见。
        let bounds = crate::capsule_window_bounds();
        if let Err(e) = capsule.set_size(bounds.width, bounds.height) {
            log::warn!("[fallback-card] restoring capsule size failed: {e}");
        }
        // 位置一样要还 —— 卡片把窗口挪到了右下角，胶囊的位置是底部居中。只还尺寸
        // 不还位置，下一次录音胶囊就出现在右下角（词条卡片在真机上踩过这个 bug）。
        // Clear layout cache before returning the capsule to dictation.
        capsule.invalidate_layout();
        if let Err(e) = capsule.position_capsule_bottom_center() {
            log::warn!("[fallback-card] restoring capsule position failed: {e}");
        }
        if let Some(payload) = deferred_capsule {
            // 卡片期间听写状态仍会推进，只是不能碰共享窗口。
            // 卡片释放后把最新状态一次性应用回来；若最新是 Idle，该 helper 会正常隐藏。
            let preferences = inner.backend.get_preferences();
            let show_capsule = preferences.show_capsule;
            let classic_style = matches!(preferences.capsule_style, CapsuleStyle::Classic);
            capsule.apply_capsule_payload(&payload, show_capsule, classic_style, true);
        }
    });
}

pub struct Coordinator {
    inner: Arc<Inner>,
}

fn shared_backend_from_stores(
    history: &HistoryStore,
    activity: &ActivityStore,
    prefs: &PreferencesStore,
    vocab: &DictionaryStore,
    app: crate::core_adapters::AppHandleSlot,
    native_asr: crate::core_adapters::TauriNativeAsrDependencies,
    hotkey_status: Arc<Mutex<HotkeyStatus>>,
) -> (
    Arc<openless_core::OpenLessBackend>,
    Arc<crate::local_llm::LocalLlamaPolisher>,
    Arc<crate::prearm::PrearmRegistry>,
) {
    let data_dir = crate::persistence::data_dir().unwrap_or_else(|error| {
        log::warn!("[core] data directory unavailable, using fallback config path: {error}");
        std::env::temp_dir().join("openless-core-fallback")
    });
    let locale = std::env::var("LANG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "en-US".to_string());
    let repositories = openless_core::BackendRepositories {
        preferences: prefs.core(),
        history: history.core(),
        activity: activity.core(),
        vocabulary: vocab.core(),
    };
    let backend_slot = crate::core_adapters::backend_slot();
    // 润色引擎的具体实例在这里创建,注册进 backend 依赖的同时保留给 Coordinator:
    // 启动预载/状态查询与听写润色共享同一个实例,不另起 backend。
    let local_polisher = Arc::new(crate::local_llm::LocalLlamaPolisher::new());
    // 预置录音注册表同样一处创建、两侧共享：热键 bridge 触发/撤销，
    // TauriAudioRecorder 接管。
    let prearm = Arc::new(crate::prearm::PrearmRegistry::new(Arc::clone(&app)));
    let dependencies = crate::core_adapters::backend_dependencies(
        app,
        Arc::clone(&backend_slot),
        native_asr,
        Arc::clone(&local_polisher),
        Arc::clone(&repositories.preferences),
        hotkey_status,
        Arc::clone(&prearm),
    );
    let backend = Arc::new(
        openless_core::OpenLessBackend::new_with_repositories(
            openless_core::BackendConfig {
                cache_dir: data_dir.join("cache"),
                data_dir,
                home_dir: std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(std::path::PathBuf::from),
                resource_dir: None,
                platform: crate::types::PlatformCapabilities::current(),
                locale,
            },
            dependencies,
            repositories,
        )
        .expect("shared backend config always has a non-empty data directory"),
    );
    *backend_slot.lock() = Some(Arc::downgrade(&backend));
    (backend, local_polisher, prearm)
}

struct Inner {
    host: crate::tauri_coordinator_host::TauriCoordinatorHost,
    backend: Arc<openless_core::OpenLessBackend>,
    /// 润色引擎的具体实例（与注册进 backend 的是同一个 Arc）：预载与状态查询用。
    local_polisher: Arc<crate::local_llm::LocalLlamaPolisher>,
    /// 预置录音注册表（与 TauriAudioRecorder 持有的是同一个 Arc）：
    /// 热键 Pressed 边沿触发建流，组合键边沿/关机撤销。
    prearm: Arc<crate::prearm::PrearmRegistry>,
    /// 实际安装在宿主上的快捷键目标。设置事务只通过显式 target 更新这里，
    /// 监听器安装/恢复不得回读尚未提交或已回滚的 preferences。
    hotkey_runtime_target: Mutex<openless_core::HotkeyRuntimeTarget>,
    /// 串行化 Tauri 侧“Core 设置事务 + 宿主 effect”以及风格包删除 effect，
    /// 防止两个命令把显式 runtime target 乱序安装。
    settings_host_gate: Mutex<()>,
    /// 建议卡片是不是正占着胶囊窗口。
    ///
    /// An inactive fallback card must leave regular dictation presentation unchanged.
    /// 开始都会去 hide 胶囊窗口，和 `emit_capsule` 的 show 抢同一个窗口。
    hotkey: Mutex<Option<HotkeyMonitor>>,
    hotkey_status: Arc<Mutex<HotkeyStatus>>,
    shortcut_recording_active: AtomicBool,
    /// 自定义组合键监听器（global-hotkey crate）。当 `prefs.hotkey.trigger == Custom` 时
    /// 代替 modifier-only 的 hotkey monitor。`None` 表示不使用自定义组合键或还没成功安装。
    combo_hotkey: Mutex<Option<ComboHotkeyMonitor>>,
    side_aware_combo: Mutex<Option<crate::side_aware_combo::SideAwareComboMonitor>>,
    /// 最近一次 emit_capsule 下发的 state，纯内省/测试用途（在 app 句柄校验之前写入，
    /// 因此无 GUI 的测试环境也能断言「按下热键 → 弹了哪种胶囊」）。写入是单次廉价
    /// 加锁，对 ~30Hz 录音回调可忽略。
    last_capsule_state: Mutex<Option<CapsuleState>>,
    /// 每次 capsule payload 递增。终态自动隐藏会带上该代数，防止旧 timer
    /// 覆盖新的听写可见状态。
    capsule_event_epoch: AtomicU64,
    /// 将 capsule 事件与自动隐藏线性化。这样一个旧 timer 要么在新的 payload 之前收起
    /// 旧提示，要么发现代数已改变直接放弃，绝不会在新会话之后补发 Idle。
    capsule_event_lock: Mutex<()>,
    /// 预备态标志：按下热键即"乐观显示"胶囊（带入场动画），此时麦克风还在 cpal
    /// init 窗口内、没有第一帧 PCM。为 true 时 emit_capsule 把 Recording payload 的
    /// `warming` 打成 true（前端渲染"待命"光效）；`level_handler` 首次触发（PCM 真的
    /// 流入）后置 false，光条"点亮"进入正式录音。begin_session 每次入场重置为 true。
    /// Coordinator 退出信号。各 hotkey supervisor loop 在每轮重试 sleep 之前会检查
    /// 此 flag；为 true 时 loop 立刻 return。生产场景里 process exit 一并 reap 所有
    /// supervisor 线程，但 integration test 和未来 RunEvent::Exit 钩子需要这条
    /// 显式退出路径。审计 3.1.2。
    shutdown: AtomicBool,
}

impl Coordinator {
    pub fn new() -> Self {
        {
            const PERSIST_DEGRADE_SUFFIX: &str = "";

            let history = HistoryStore::new().unwrap_or_else(|e| {
                log::error!(
                    "[coord] HistoryStore init failed: {e}; 降级为空历史记录{PERSIST_DEGRADE_SUFFIX}"
                );
                HistoryStore::new_fallback()
            });
            let prefs = PreferencesStore::new().unwrap_or_else(|e| {
                log::error!(
                    "[coord] PreferencesStore init failed: {e}; 降级为默认偏好设置{PERSIST_DEGRADE_SUFFIX}"
                );
                PreferencesStore::new_fallback()
            });
            let vocab = DictionaryStore::new().unwrap_or_else(|e| {
                log::error!("[coord] DictionaryStore init failed: {e}; using empty vocabulary");
                DictionaryStore::new_fallback()
            });

            let activity = ActivityStore::load().unwrap_or_else(|e| {
                log::error!("[coord] ActivityStore init failed: {e}; 活动计数降级为内存态");
                ActivityStore::new_fallback()
            });

            let app = crate::core_adapters::app_handle_slot();
            let native_asr = crate::core_adapters::TauriNativeAsrDependencies::new();
            let hotkey_status = Arc::new(Mutex::new(HotkeyStatus::default()));
            let (backend, local_polisher, prearm) = shared_backend_from_stores(
                &history,
                &activity,
                &prefs,
                &vocab,
                Arc::clone(&app),
                native_asr.clone(),
                Arc::clone(&hotkey_status),
            );

            let host = crate::tauri_coordinator_host::TauriCoordinatorHost::new(Arc::clone(&app));
            let hotkey_runtime_target = (&backend.get_preferences()).into();
            let inner = Arc::new(Inner {
                host,
                backend,
                local_polisher,
                prearm,
                hotkey_runtime_target: Mutex::new(hotkey_runtime_target),
                settings_host_gate: Mutex::new(()),
                hotkey: Mutex::new(None),
                hotkey_status,
                shortcut_recording_active: AtomicBool::new(false),
                combo_hotkey: Mutex::new(None),
                side_aware_combo: Mutex::new(None),
                last_capsule_state: Mutex::new(None),
                capsule_event_epoch: AtomicU64::new(0),
                capsule_event_lock: Mutex::new(()),
                shutdown: AtomicBool::new(false),
            });
            Self { inner }
        }
    }

    pub fn backend(&self) -> Arc<openless_core::OpenLessBackend> {
        Arc::clone(&self.inner.backend)
    }

    /// 润色引擎共享实例（注册进 backend 的同一个 Arc）。
    pub fn local_polisher(&self) -> Arc<crate::local_llm::LocalLlamaPolisher> {
        Arc::clone(&self.inner.local_polisher)
    }

    /// 启动预载：固定本地模型（ASR=Qwen3-ASR 1.7B MLX worker；LLM=Gemma 4 GGUF）
    /// 在后台载入并常驻。主窗口显示与静默开机（托盘启动）都执行；两个任务相互
    /// 独立、失败只记日志不阻塞 UI。共享实例：LLM 走 Coordinator 持有的同一个
    /// LocalLlamaPolisher（与润色同 slot/锁）；ASR 走 backend 的 local_asr
    /// prepare（与听写共享同一个 Qwen3MlxRuntime Arc）。听写触发的懒加载与预载
    /// 在 runtime 自己的锁上汇合，不会载入第二份模型。
    pub fn preload_local_models(&self) {
        // LLM：固定 Gemma 模型预载；状态变化经 watcher 广播 local-llm:engine-changed。
        {
            let polisher = self.local_polisher();
            // 状态回调在 async runtime 工作线程上触发（`preload()` 与润色的状态机都在
            // 那里推进）。回调里只复制纯数据的 status；`AppHandle` 从宿主槽位借出、
            // 克隆与释放都发生在 event-loop 线程。
            let host = self.inner.host.clone();
            polisher.set_status_watcher(Box::new(move |status| {
                let host_for_main = host.clone();
                let status = status.clone();
                host.on_main_thread(move || {
                    // 槽位锁只在克隆这一句持有：`if let` 的临时值会活到整个 body，
                    // 持锁 emit 会让别的工作线程卡在槽位上。
                    let app = { host_for_main.app_handle_slot().lock().clone() };
                    if let Some(app) = app {
                        let _ = tauri::Emitter::emit(&app, "local-llm:engine-changed", &status);
                    }
                });
            }));
            tauri::async_runtime::spawn(async move {
                match polisher.preload().await {
                    Ok(()) => {
                        log::info!("[preload] local LLM resident (shared polisher slot)");
                    }
                    Err(error) => {
                        log::warn!("[preload] local LLM preload failed: {error}");
                    }
                }
            });
        }
        // ASR：prepare 即真实加载（共享 Qwen3MlxRuntime）；完成/失败 core 都会广播
        // local-asr:engine-changed。模型目录缺失时错误会显式出现在状态里。
        {
            let backend = self.backend();
            tauri::async_runtime::spawn(async move {
                match backend.services().local_asr.prepare().await {
                    Ok(model) => {
                        log::info!("[preload] local ASR engine resident: {model}");
                    }
                    Err(error) => {
                        log::warn!("[preload] local ASR prepare failed: {error}");
                    }
                }
            });
        }
    }

    pub fn show_core_insert_fallback(&self, text: String, reason: &str) {
        let reason = match reason {
            "partial_stream" => crate::types::INSERT_FALLBACK_REASON_PARTIAL_STREAM,
            _ => crate::types::INSERT_FALLBACK_REASON_INSERT_FAILED,
        };
        show_insert_fallback_card(&self.inner, text, reason);
    }

    pub fn present_core_capsule(&self, payload: CapsulePayload) {
        let _ = self.present_core_capsule_if_current(payload, None);
    }

    pub(crate) fn present_core_capsule_if_current(
        &self,
        payload: CapsulePayload,
        expected_epoch: Option<u64>,
    ) -> Option<u64> {
        let state = payload.state;
        // Core 已拥有本帧的翻译、准备态和会话归属；不可在窗口层按迟到的
        // 当前状态重新拼装，否则冷启动或快速切换会丢失真实反馈。
        let epoch = emit_core_capsule(&self.inner, payload, expected_epoch)?;
        if let Some(delay_ms) = core_capsule_hide_delay(state) {
            let inner = Arc::clone(&self.inner);
            self.inner.host.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                hide_core_capsule_if_current(&inner, epoch);
            });
        }
        Some(epoch)
    }

    pub(crate) fn tauri_host(&self) -> crate::tauri_coordinator_host::TauriCoordinatorHost {
        self.inner.host.clone()
    }

    /// 让所有 hotkey supervisor loop（dictation / qa / combo /
    /// open_app）在下一轮 sleep / poll
    /// 后退出。生产场景下进程退出
    /// 一并 reap 所有线程，但 integration test 和未来 RunEvent::Exit 钩子需要
    /// 显式退出路径。审计 3.1.2。
    #[allow(dead_code)]
    pub fn request_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.prearm.cancel();
    }

    pub fn start_hotkey_listener(&self) {
        // 起一个守护线程，反复尝试安装 hotkey hook。Accessibility 一被授予就立即生效，
        // 用户不需要手动重启 OpenLess。
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("openless-hotkey-supervisor".into())
            .spawn(move || hotkey_supervisor_loop(inner))
            .ok();
    }

    pub fn stop_hotkey_listener(&self) {
        self.inner.hotkey.lock().take();
    }
    pub fn start_combo_hotkey_listener(&self) {
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("openless-combo-hotkey-supervisor".into())
            .spawn(move || combo_hotkey_supervisor_loop(inner))
            .ok();
    }

    pub fn stop_combo_hotkey_listener(&self) {
        take_combo_hotkey_on_main_thread(&self.inner);
    }

    /// 用户在设置里改了自定义组合键时调用。
    pub(crate) fn update_combo_hotkey_binding(&self) {
        let target = hotkey_runtime_target(&self.inner);
        if crate::shortcut_binding::legacy_modifier_trigger(&target.dictation).is_some() {
            take_combo_hotkey_on_main_thread(&self.inner);
            self.inner.side_aware_combo.lock().take();
            log::info!("[coord] combo hotkey 已关闭（modifier-only）");
            return;
        }
        let binding = target.dictation;
        if is_unconfigured_shortcut(&binding) {
            take_combo_hotkey_on_main_thread(&self.inner);
            self.inner.side_aware_combo.lock().take();
            log::info!("[coord] combo hotkey 已关闭（无绑定）");
            return;
        }

        if crate::shortcut_binding::binding_requires_side_aware_hook(&binding) {
            take_combo_hotkey_on_main_thread(&self.inner);
            self.inner.side_aware_combo.lock().take();
            let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
            match crate::side_aware_combo::SideAwareComboMonitor::start(binding, tx) {
                Ok(monitor) => {
                    *self.inner.side_aware_combo.lock() = Some(monitor);
                    let bridge_inner = Arc::clone(&self.inner);
                    std::thread::Builder::new()
                        .name("openless-side-combo-bridge".into())
                        .spawn(move || combo_hotkey_bridge_loop(bridge_inner, rx))
                        .ok();
                    log::info!("[coord] side-aware combo hotkey listener installed (via update)");
                }
                Err(e) => {
                    log::warn!("[coord] update side-aware combo binding 失败: {e}");
                }
            }
            return;
        }

        self.inner.side_aware_combo.lock().take();
        let inner_clone = Arc::clone(&self.inner);
        let binding_for_main = binding.clone();
        if self
            .inner
            .host
            .run_on_main_thread(move || {
                if let Some(monitor) = inner_clone.combo_hotkey.lock().as_ref() {
                    if let Err(e) = monitor.update_binding(binding_for_main.clone()) {
                        log::warn!("[coord] update combo hotkey binding 失败: {e}");
                    }
                    return;
                }
                let (tx, rx) = mpsc::channel::<ComboHotkeyEvent>();
                match ComboHotkeyMonitor::start(binding_for_main, tx) {
                    Ok(monitor) => {
                        *inner_clone.combo_hotkey.lock() = Some(monitor);
                        log::info!(
                            "[coord] combo hotkey listener installed on main thread (via update)"
                        );
                        let bridge_inner = Arc::clone(&inner_clone);
                        std::thread::Builder::new()
                            .name("openless-combo-hotkey-bridge".into())
                            .spawn(move || combo_hotkey_bridge_loop(bridge_inner, rx))
                            .ok();
                    }
                    Err(e) => {
                        log::warn!("[coord] update combo hotkey binding 失败: {e}");
                    }
                }
            })
            .is_err()
        {
            log::warn!("[coord] update combo hotkey binding: AppHandle 未 bind，跳过");
        }
    }

    /// 设置保存后立即把胶囊样式同步进 Tauri Host 缓存。
    /// emit_capsule 的 ~30Hz 主线程闭包本来也会同步，但入场帧的 payload 是在闭包
    /// 同步之前克隆的（会带一帧旧样式），且 Windows 上主线程拥塞时闭包可能延迟
    /// 执行——用户反馈「切换成默认风格后仍显示流光 Siri」。在保存路径直接同步后，
    /// 任何平台的下一次录音从入场帧起就携带最新样式，不再依赖 emit 闭包的时序。
    pub fn sync_capsule_style_from_preferences(&self) {
        self.inner
            .host
            .cache_capsule_style(self.inner.backend.get_preferences().capsule_style);
    }
    /// 落字失败兜底卡片自己关掉了（用户点关闭 / TTL 到时）。
    pub fn dismiss_insert_fallback_card(&self) {
        hide_insert_fallback_card(&self.inner);
    }

    /// 在 event-loop 线程上执行 `task`。
    ///
    /// 供异步 `#[tauri::command]`（跑在 async runtime 工作线程上）把碰 Tauri 句柄的
    /// 收尾工作交回主线程：宿主槽位里的 `AppHandle` 只借用不克隆，工作线程上投递后
    /// 立即返回（本来就在主线程时同步执行）。未 bind 或投递失败时返回错误串，
    /// 由调用方决定是否忽略。
    pub(crate) fn run_on_main_thread<F>(&self, task: F) -> Result<(), String>
    where
        F: FnOnce() + Send + 'static,
    {
        self.inner.host.run_on_main_thread(task)
    }

    pub fn report_insert_fallback_card_height(
        &self,
        presentation_id: u64,
        height: f64,
    ) -> Result<(), String> {
        report_insert_fallback_card_height(&self.inner, presentation_id, height)
    }

    pub(crate) fn update_hotkey_binding(&self) {
        let target = hotkey_runtime_target(&self.inner);
        let dictation_trigger = crate::shortcut_binding::legacy_modifier_trigger(&target.dictation);
        let binding = crate::types::HotkeyBinding {
            trigger: dictation_trigger.unwrap_or(crate::types::HotkeyTrigger::Custom),
            keys: None,
        };
        if dictation_trigger.is_some() {
            take_combo_hotkey_on_main_thread(&self.inner);
        } else {
            self.update_combo_hotkey_binding();
        }
        self.ensure_modifier_hotkey_monitor(binding);
        self.update_modifier_shortcut_bindings();
    }

    fn ensure_modifier_hotkey_monitor(&self, binding: crate::types::HotkeyBinding) {
        if let Some(monitor) = self.inner.hotkey.lock().as_ref() {
            monitor.update_binding(binding);
            return;
        }
        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let combo_tx = spawn_combo_abort_bridge(&self.inner, handle_trigger_combined);
        match HotkeyMonitor::start(binding, tx, combo_tx) {
            Ok(monitor) => {
                let adapter = monitor.kind();
                *self.inner.hotkey.lock() = Some(monitor);
                *self.inner.hotkey_status.lock() = HotkeyStatus {
                    adapter,
                    state: HotkeyStatusState::Installed,
                    message: Some(format!("{} 已安装", adapter.display_name())),
                    last_error: None,
                };
                let inner_clone = Arc::clone(&self.inner);
                std::thread::Builder::new()
                    .name("openless-hotkey-bridge".into())
                    .spawn(move || hotkey_bridge_loop(inner_clone, rx))
                    .ok();
            }
            Err(e) => {
                *self.inner.hotkey_status.lock() = HotkeyStatus {
                    adapter: HotkeyMonitor::capability().adapter,
                    state: HotkeyStatusState::Failed,
                    message: Some(e.message.clone()),
                    last_error: Some(e),
                };
            }
        }
    }

    pub fn update_modifier_shortcut_bindings(&self) {
        if self.inner.hotkey.lock().as_ref().is_some() {}
    }

    /// 将 Core 已校验并完成冲突协调的显式目标应用到宿主监听器。
    ///
    /// 本方法不会读取 preferences；失败时 target 保持为 `next`，由 Core 根据
    /// receipt 调用反向 change 恢复，从而让部分安装也能收敛回旧状态。
    pub(crate) fn apply_hotkey_runtime_change(
        &self,
        change: &openless_core::SettingsValueChange<openless_core::HotkeyRuntimeTarget>,
    ) -> Result<(), String> {
        let previous = &change.previous;
        let next = &change.next;
        *self.inner.hotkey_runtime_target.lock() = next.clone();
        if previous.dictation != next.dictation {
            self.update_hotkey_binding();
            self.update_combo_hotkey_binding();
        }
        Ok(())
    }

    pub(crate) fn lock_settings_host(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.inner.settings_host_gate.lock()
    }

    pub(crate) async fn cancel_active_voice(&self) {
        dictation::cancel_active_session(&self.inner).await;
    }

    pub(crate) async fn cancel_dictation_from_cli(
        &self,
    ) -> Result<openless_core::CliDispatchOutcome, openless_core::BackendError> {
        self.inner
            .backend
            .dispatch_cli_intent(crate::cli::CliIntent::CancelDictation)
            .await
    }

    pub fn hotkey_capability(&self) -> HotkeyCapability {
        HotkeyMonitor::capability()
    }

    pub fn set_shortcut_recording_active(&self, active: bool) {
        self.inner
            .shortcut_recording_active
            .store(active, Ordering::SeqCst);
        // 同步给热键监听器：录制态激活时 CGEventTap 上报 Fn 按下边沿，
        // 供前端 ShortcutRecorder 提交 Fn 绑定（浏览器不向网页层下发 Fn keydown）。
        let sync_ok = self.inner.hotkey.lock().as_ref().map(|m| {
            m.set_recording_active(active);
            true
        });
        if active {
            reset_shortcut_held_state(&self.inner);
        }
        log::info!(
            "[coord] shortcut recording active={active} (synced_to_hotkey={})",
            sync_ok.unwrap_or(false)
        );
    }
}

/// 成功胶囊的停留时长刻意对齐热键的收尾冷却（hotkey_interpreter
/// TERMINAL_COOLDOWN = 150ms）：胶囊消失的时刻就是下一次录音已经可以触发的
/// 时刻，不再悬停占屏。失败保持 2 秒——没上屏的结果必须让人看清。
const CAPSULE_DONE_HIDE_DELAY_MS: u64 = 150;
const CAPSULE_ERROR_HIDE_DELAY_MS: u64 = 2000;

/// Core 终态事件只描述语义，原生胶囊保持多久由 Tauri Host 负责。把映射集中成纯函数，
/// 可以明确锁定成功、失败、取消三条时序，并让测试不依赖真实窗口与计时器。
fn core_capsule_hide_delay(state: CapsuleState) -> Option<u64> {
    match state {
        CapsuleState::Done => Some(CAPSULE_DONE_HIDE_DELAY_MS),
        CapsuleState::Error => Some(CAPSULE_ERROR_HIDE_DELAY_MS),
        CapsuleState::Cancelled => Some(0),
        _ => None,
    }
}

#[cfg(test)]
mod core_capsule_tests {
    use super::*;

    #[test]
    fn core_terminal_capsule_timing_preserves_the_legacy_contract() {
        // 成功胶囊在热键冷却结束的同一时刻消失（150ms），失败仍停留 2 秒。
        assert_eq!(core_capsule_hide_delay(CapsuleState::Done), Some(150));
        assert_eq!(core_capsule_hide_delay(CapsuleState::Error), Some(2000));
        assert_eq!(core_capsule_hide_delay(CapsuleState::Cancelled), Some(0));
        assert_eq!(core_capsule_hide_delay(CapsuleState::Recording), None);
    }
}
