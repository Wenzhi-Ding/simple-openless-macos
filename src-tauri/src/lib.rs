//! OpenLess Tauri backend.
//!
//! Modules mirror the original Swift libraries (one purpose per file):
//! - hotkey: global hotkey monitor
//! - recorder: microphone capture (16 kHz mono Int16 PCM)
//! - asr: streaming ASR providers (Volcengine SAUC bigmodel)
//! - polish: OpenAI-compatible chat completions client
//! - insertion: cursor-position text insertion (AX / paste)
//! - persistence: history + preferences + credentials vault
//! - coordinator: dictation state machine glue
//! - commands: Tauri IPC surface
mod asr;
mod audio_mute;
#[cfg(test)]
mod build_target;
mod cli;
mod combo_hotkey;
mod commands;
mod coordinator;
mod core_adapters;
mod device_watch;
mod focus_target;
mod global_hotkey_runtime;
mod host_document;
mod hotkey;
mod hotwords;
mod insertion;
mod local_llm;
mod ocr_engine;
mod permissions;
mod persistence;
mod recorder;
mod screen_ocr;
mod shortcut_binding;
mod side_aware_combo;
mod tauri_coordinator_host;
mod tauri_events;
mod types;
mod unicode_keystroke;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
const LOG_ROTATE_LIMIT_BYTES: u64 = 10 * 1024 * 1024;
static TRAY_MICROPHONE_WATCHER_STOPPING: AtomicBool = AtomicBool::new(false);
struct TrayMicrophoneDeviceCache(parking_lot::Mutex<Vec<recorder::MicrophoneDevice>>);
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, Submenu, SubmenuBuilder,
};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, RunEvent, Runtime};
#[cfg(test)]
pub(crate) fn set_backend_preferences_for_test(
    backend: &openless_core::OpenLessBackend,
    preferences: crate::types::UserPreferences,
) {
    backend
        .update_settings(
            preferences,
            openless_core::SettingsUpdateOptions::STRICT,
            &openless_core::NoopSettingsRuntime,
        )
        .expect("test preferences must satisfy the public settings contract");
}
pub fn run() {
    run_desktop();
}
macro_rules! app_invoke_handler_desktop {
    () => {
        tauri::generate_handler![
            commands::get_startup_snapshot,
            commands::get_settings,
            commands::local_llm_model_status,
            commands::set_settings,
            commands::get_hotkey_status,
            commands::get_hotkey_capability,
            commands::set_shortcut_recording_active,
            commands::get_platform_capabilities,
            commands::list_microphone_devices,
            commands::start_microphone_level_monitor,
            commands::stop_microphone_level_monitor,
            commands::list_history,
            commands::delete_history_entry,
            commands::clear_history,
            commands::get_activity_stats,
            commands::list_vocab,
            commands::add_vocab,
            commands::remove_vocab,
            commands::set_vocab_enabled,
            commands::copy_text_to_clipboard,
            commands::dismiss_insert_fallback_card,
            commands::report_insert_fallback_card_height,
            commands::read_audio_recording,
            commands::export_audio_recording,
            commands::retranscribe_recording,
            commands::stop_dictation,
            commands::cancel_dictation,
            commands::repolish,
            commands::check_accessibility_permission,
            commands::request_accessibility_permission,
            commands::check_microphone_permission,
            commands::check_screen_capture_permission,
            commands::request_screen_capture_permission,
            commands::request_microphone_permission,
            commands::open_system_settings,
            commands::validate_shortcut_binding,
            commands::set_dictation_hotkey,
            commands::local_asr_engine_status,
            commands::export_error_log,
            commands::debug_read_cursor_context,
        ]
    };
}
fn run_desktop() {
    let coordinator = Arc::new(coordinator::Coordinator::new());
    let core_backend = coordinator.backend();
    let builder = tauri::Builder::default();
    let builder = builder.plugin(tauri_nspanel::init());
    builder
        .plugin(
            tauri_plugin_single_instance::init(|app, argv, _cwd| {
                if let Some(intent) = cli::parse_cli_intent(&argv) {
                    log::info!(
                        "[single-instance] another instance launched with intent={intent:?}, dispatching"
                    );
                    dispatch_cli_intent(app, intent);
                    return;
                }
                if let Some(coordinator) = app
                    .try_state::<Arc<coordinator::Coordinator>>()
                    .map(|s| Arc::clone(&*s))
                {
                    if coordinator.backend().get_preferences().start_minimized {
                        log::info!(
                            "[single-instance] start_minimized=true → skipping show on relaunch"
                        );
                        return;
                    }
                }
                log::info!(
                    "[single-instance] another instance launched, focusing existing main window"
                );
                show_main_window(app);
            }),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ),
        )
        .manage(coordinator.clone())
        .manage(core_backend.clone())
        .manage(commands::MicrophoneMonitorState::new(None))
        .manage(commands::TrayMicrophoneMenuState::new(Vec::new()))
        .manage(TrayMicrophoneDeviceCache(parking_lot::Mutex::new(Vec::new())))
        .setup(move |app| {
            init_file_logger();
            log::info!("=== OpenLess 启动 ===");
            if let Some(capsule) = app.get_webview_window("capsule") {
                {
                    use tauri_nspanel::cocoa::appkit::NSWindowCollectionBehavior;
                    use tauri_nspanel::WebviewWindowExt;
                    match capsule.to_panel() {
                        Ok(panel) => {
                            const NS_NONACTIVATING_PANEL_MASK: i32 = 1 << 7;
                            panel.set_style_mask(NS_NONACTIVATING_PANEL_MASK);
                            panel.set_level(25);
                            panel
                                .set_collection_behaviour(
                                    NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary
                                        | NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces,
                                );
                        }
                        Err(e) => log::warn!("[capsule] to_panel failed: {e:?}"),
                    }
                }
                if true {
                    if let Err(e) = capsule.set_ignore_cursor_events(true) {
                        log::warn!("[capsule] set_ignore_cursor_events failed: {e}");
                    }
                }
                if let Err(e) = position_capsule_bottom_center(&capsule) {
                    log::warn!("[capsule] position failed: {e}");
                }
                let _ = capsule.hide();
            }
            if let Some(main) = app.get_webview_window("main") {
                {
                    use window_vibrancy::{
                        apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
                    };
                    if let Err(e) = main.set_decorations(true) {
                        log::warn!("[main] enable native decorations failed: {e}");
                    }
                    if let Err(e) = apply_vibrancy(
                        &main,
                        NSVisualEffectMaterial::HudWindow,
                        Some(NSVisualEffectState::Active),
                        Some(20.0),
                    ) {
                        log::warn!("[main] vibrancy failed: {e}");
                    }
                }
                let force_show = std::env::var("OPENLESS_SHOW_MAIN_ON_START")
                    .ok()
                    .as_deref() == Some("1");
                let suppress_show = !force_show
                    && coordinator.backend().get_preferences().start_minimized;
                if suppress_show {
                    log::info!(
                        "[main] start_minimized=true → 跳过初始 show，等用户点托盘"
                    );
                } else {
                    if let Err(e) = main.show() {
                        log::warn!("[main] initial show failed: {e}");
                    }
                }
            }
            let tray_menu = build_tray_menu(app, &coordinator)?;
            let menu = tray_menu.menu;
            if let Some(icon) = app.default_window_icon() {
                {
                    let state = app.state::<commands::TrayMicrophoneMenuState>();
                    *state.lock() = tray_menu.microphone_items;
                }
                let _tray = TrayIconBuilder::with_id("main-tray")
                    .icon(icon.clone())
                    .icon_as_template(false)
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "toggle" => show_main_window(app),
                        "quit" => app.exit(0),
                        id => {
                            handle_microphone_tray_menu_event(app, id);
                        }
                    })
                    .on_tray_icon_event(move |tray, event| match event {
                        TrayIconEvent::Enter { .. } => {
                            if let Err(err) = refresh_tray_microphone_menu(
                                tray.app_handle(),
                            ) {
                                log::warn!(
                                    "[tray] refresh microphone menu on hover failed: {err}"
                                );
                            }
                        }
                        TrayIconEvent::Click { button: MouseButton::Left, .. } => {
                            show_main_window(tray.app_handle())
                        }
                        _ => {}
                    })
                    .build(app)?;
                start_tray_microphone_watcher(app.handle().clone());
            } else {
                log::warn!("[startup] default window icon missing; tray icon disabled");
            }
            let app_handle = app.handle().clone();
            coordinator.tauri_host().bind(app_handle);
            crate::tauri_events::start(app.handle().clone(), Arc::clone(&core_backend));
            coordinator.start_hotkey_listener();
            coordinator.preload_local_models();
            if std::env::var("OPENLESS_SHOW_MAIN_ON_START").ok().as_deref() == Some("1")
            {
                show_main_window(app.handle());
            }
            let first_run_args: Vec<String> = std::env::args().collect();
            if let Some(intent) = cli::parse_cli_intent(&first_run_args) {
                log::info!("[startup] first-run CLI intent={intent:?}, dispatching");
                dispatch_cli_intent(app.handle(), intent);
            }
            Ok(())
        })
        .invoke_handler(app_invoke_handler_desktop!())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            RunEvent::Ready => {
                let coordinator = app.state::<Arc<coordinator::Coordinator>>();
                coordinator.start_combo_hotkey_listener();
            }
            RunEvent::Reopen { .. } => show_main_window(app),
            RunEvent::WindowEvent { label, event, .. } => {
                if label == "main" {
                    if let tauri::WindowEvent::CloseRequested { ref api, .. } = event {
                        api.prevent_close();
                        hide_main_window(app);
                    }
                }
            }
            RunEvent::Exit => {
                TRAY_MICROPHONE_WATCHER_STOPPING.store(true, Ordering::Relaxed);
                let coordinator = app.state::<Arc<coordinator::Coordinator>>();
                coordinator.stop_hotkey_listener();
                coordinator.stop_combo_hotkey_listener();
                // Must be synchronous: the process finalises as soon as this
                // callback returns, so a spawned task does not get a chance to
                // run. llama.cpp keeps Metal buffers alive until the model and
                // backend are dropped, and the ggml rsets assertion aborts the
                // process when they are still held at exit.
                coordinator.local_polisher().unload_blocking();
                let backend = coordinator.backend();
                tauri::async_runtime::spawn(async move {
                    let _ = backend.shutdown().await;
                });
            }
            _ => {}
        });
}
struct MicrophoneTrayMenu {
    submenu: Submenu<tauri::Wry>,
    items: Vec<commands::TrayMicrophoneMenuItem>,
}
struct TrayMenu {
    menu: Menu<tauri::Wry>,
    microphone_items: Vec<commands::TrayMicrophoneMenuItem>,
}
#[derive(Debug, Clone, Copy)]
struct TrayLabels {
    toggle: &'static str,
    microphone: &'static str,
    default_microphone: &'static str,
    no_microphones: &'static str,
    default_device_suffix: &'static str,
    quit: &'static str,
}
impl TrayLabels {
    fn for_locale(locale: &str) -> Self {
        match locale {
            "en" => Self {
                toggle: "Show main window",
                microphone: "Select microphone",
                default_microphone: "System default microphone",
                no_microphones: "No microphones found",
                default_device_suffix: " (System default)",
                quit: "Quit OpenLess",
            },
            "zh-TW" => Self {
                toggle: "顯示主視窗",
                microphone: "選擇麥克風",
                default_microphone: "系統預設麥克風",
                no_microphones: "找不到麥克風",
                default_device_suffix: "（系統預設）",
                quit: "退出 OpenLess",
            },
            "ja" => Self {
                toggle: "メインウィンドウを表示",
                microphone: "マイクを選択",
                default_microphone: "システムのデフォルトマイク",
                no_microphones: "マイクが見つかりません",
                default_device_suffix: "（システムのデフォルト）",
                quit: "OpenLessを終了",
            },
            "ko" => Self {
                toggle: "메인 창 표시",
                microphone: "마이크 선택",
                default_microphone: "시스템 기본 마이크",
                no_microphones: "마이크를 찾을 수 없음",
                default_device_suffix: "（시스템 기본）",
                quit: "OpenLess 종료",
            },
            _ => Self {
                toggle: "显示主窗口",
                microphone: "选择麦克风",
                default_microphone: "系统默认麦克风",
                no_microphones: "未发现麦克风",
                default_device_suffix: "（系统默认）",
                quit: "退出 OpenLess",
            },
        }
    }
    fn default_device_label(self, device_name: &str) -> String {
        format!("{device_name}{}", self.default_device_suffix)
    }
}
fn build_tray_menu<M: Manager<tauri::Wry>>(
    app: &M,
    coordinator: &Arc<coordinator::Coordinator>,
) -> tauri::Result<TrayMenu> {
    let locale = std::env::var("LANG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "zh-CN".to_string());
    let labels = TrayLabels::for_locale(&locale);
    let toggle = MenuItemBuilder::with_id("toggle", labels.toggle).build(app)?;
    let microphone_menu = build_microphone_tray_menu(app, coordinator, labels)?;
    let quit = MenuItemBuilder::with_id("quit", labels.quit).build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&toggle, &microphone_menu.submenu, &quit])
        .build()?;
    Ok(TrayMenu {
        menu,
        microphone_items: microphone_menu.items,
    })
}
fn build_microphone_tray_menu<M: Manager<tauri::Wry>>(
    app: &M,
    coordinator: &Arc<coordinator::Coordinator>,
    labels: TrayLabels,
) -> tauri::Result<MicrophoneTrayMenu> {
    let selected = coordinator
        .backend()
        .get_preferences()
        .microphone_device_name;
    let mut items = Vec::new();
    let mut submenu = SubmenuBuilder::with_id(app, "microphone", labels.microphone);
    let devices = app.state::<TrayMicrophoneDeviceCache>().0.lock().clone();
    let selected_available =
        selected.trim().is_empty() || devices.iter().any(|device| device.name == selected);
    let default_item = CheckMenuItemBuilder::with_id("mic-default", labels.default_microphone)
        .checked(selected.trim().is_empty() || !selected_available)
        .build(app)?;
    submenu = submenu.item(&default_item);
    items.push(commands::TrayMicrophoneMenuItem {
        id: "mic-default".to_string(),
        device_name: String::new(),
        item: default_item,
    });
    if devices.is_empty() {
        let empty = MenuItemBuilder::with_id("mic-empty", labels.no_microphones)
            .enabled(false)
            .build(app)?;
        submenu = submenu.item(&empty);
    } else {
        for (index, device) in devices.into_iter().enumerate() {
            let id = format!("mic-device-{index}");
            let label = if device.is_default {
                labels.default_device_label(&device.name)
            } else {
                device.name.clone()
            };
            let item = CheckMenuItemBuilder::with_id(&id, label)
                .checked(selected == device.name)
                .build(app)?;
            submenu = submenu.item(&item);
            items.push(commands::TrayMicrophoneMenuItem {
                id,
                device_name: device.name,
                item,
            });
        }
    }
    Ok(MicrophoneTrayMenu {
        submenu: submenu.build()?,
        items,
    })
}
pub(crate) fn refresh_tray_microphone_menu(app: &AppHandle) -> tauri::Result<()> {
    let coordinator = app.state::<Arc<coordinator::Coordinator>>();
    let tray_menu = build_tray_menu(app, &coordinator)?;
    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_menu(Some(tray_menu.menu))?;
    }
    let state = app.state::<commands::TrayMicrophoneMenuState>();
    *state.lock() = tray_menu.microphone_items;
    Ok(())
}
fn microphone_devices_with_signature(
) -> Option<(Vec<recorder::MicrophoneDevice>, Vec<(String, bool)>)> {
    match recorder::list_input_devices() {
        Ok(devices) => {
            let signature = devices
                .iter()
                .map(|device| (device.name.clone(), device.is_default))
                .collect();
            Some((devices, signature))
        }
        Err(err) => {
            log::warn!("[tray] watch microphone devices failed: {err}");
            None
        }
    }
}
/// Enumerate devices off the main thread, update the shared cache, then rebuild the tray on
/// AppKit's main thread only when the device signature changed.
fn refresh_microphone_cache_if_changed(
    app: &AppHandle,
    last_signature: &parking_lot::Mutex<Option<Vec<(String, bool)>>>,
) {
    let Some((devices, signature)) = microphone_devices_with_signature() else {
        return;
    };
    {
        let mut guard = last_signature.lock();
        if guard.as_ref() == Some(&signature) {
            return;
        }
        *guard = Some(signature);
    }
    *app.state::<TrayMicrophoneDeviceCache>().0.lock() = devices;
    let refresh_app = app.clone();
    if let Err(err) = app.run_on_main_thread(move || refresh_microphone_on_main(&refresh_app)) {
        log::warn!("[tray] dispatch microphone cache refresh failed: {err}");
    }
}
/// 在主线程上刷新托盘麦克风子菜单并通知前端。供 OS 原生设备变更回调与慢速兜底轮询
/// 共用同一条收尾路径。已在主线程或被 `run_on_main_thread` 派发后调用。
fn refresh_microphone_on_main(app: &AppHandle) {
    if let Err(err) = refresh_tray_microphone_menu(app) {
        log::warn!("[tray] refresh microphone menu after device change failed: {err}");
    }
    tauri_events::publish(
        app,
        None,
        openless_core::BackendEventKind::MicrophoneDevicesChanged,
    );
}
/// 设备变更去抖闭包：被 OS 原生通知回调（macOS CoreAudio / Windows MMDevice）调用。
/// OS 回调仅调度一个后台枚举任务，绝不在 AppKit 主线程或 CoreAudio/COM 通知线程中
/// 直接枚举设备。并发通知通过 `refresh_in_flight` 合并，签名去抖避免重复刷新菜单。
fn make_microphone_change_handler(app: AppHandle) -> impl Fn() + Send + Sync + 'static {
    let last_signature = Arc::new(parking_lot::Mutex::new(None));
    let refresh_in_flight = Arc::new(AtomicBool::new(false));
    move || {
        if refresh_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let refresh_app = app.clone();
        let refresh_signature = Arc::clone(&last_signature);
        let refresh_flag = Arc::clone(&refresh_in_flight);
        if let Err(err) = std::thread::Builder::new()
            .name("openless-tray-mic-event".into())
            .spawn(move || {
                refresh_microphone_cache_if_changed(&refresh_app, &refresh_signature);
                refresh_flag.store(false, Ordering::Release);
            })
        {
            refresh_in_flight.store(false, Ordering::Release);
            log::warn!("[tray] start microphone event refresh failed: {err}");
        }
    }
}
fn start_tray_microphone_watcher(app: AppHandle) {
    TRAY_MICROPHONE_WATCHER_STOPPING.store(false, Ordering::Relaxed);
    let native_registered = device_watch::spawn_native_watcher(
        app.clone(),
        make_microphone_change_handler(app.clone()),
    );
    if native_registered {
        log::info!("[tray] OS native microphone device watcher registered");
    } else {
        log::info!(
            "[tray] no OS native microphone device watcher (unsupported platform or registration failed); relying on slow poll fallback"
        );
    }
    if let Err(err) = std::thread::Builder::new()
        .name("openless-tray-mic-poll".into())
        .spawn(move || {
            let last_signature = parking_lot::Mutex::new(None);
            refresh_microphone_cache_if_changed(&app, &last_signature);
            while !TRAY_MICROPHONE_WATCHER_STOPPING.load(Ordering::Relaxed) {
                for _ in 0..60 {
                    if TRAY_MICROPHONE_WATCHER_STOPPING.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                if TRAY_MICROPHONE_WATCHER_STOPPING.load(Ordering::Relaxed) {
                    break;
                }
                refresh_microphone_cache_if_changed(&app, &last_signature);
            }
        })
    {
        log::warn!("[tray] start microphone poll fallback failed: {err}");
    }
}
fn handle_microphone_tray_menu_event(app: &AppHandle, id: &str) {
    let tray_items = app.state::<commands::TrayMicrophoneMenuState>();
    let items = tray_items.lock();
    let Some(selected) = items.iter().find(|item| item.id == id) else {
        return;
    };
    let coord = app.state::<Arc<coordinator::Coordinator>>();
    if let Err(err) = coord
        .backend()
        .select_microphone_device(selected.device_name.clone())
    {
        log::warn!("[tray] save microphone preference failed: {err}");
        return;
    }
    commands::sync_tray_microphone_selection(&items, &selected.device_name);
}
/// 把日志同时写到 stderr + ~/Library/Logs/OpenLess/openless.log（match Swift `Log.swift`）。
pub(crate) fn init_file_logger() {
    use simplelog::{
        ColorChoice, CombinedLogger, ConfigBuilder, LevelFilter, TermLogger, TerminalMode,
        WriteLogger,
    };
    let log_dir = log_dir_path();
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "[logger] WARN create log dir failed path={}: {e}",
            log_dir.display()
        );
    }
    let log_file = log_dir.join("openless.log");
    if let Err(e) = rotate_log_if_too_large(&log_file) {
        eprintln!("[logger] WARN 日志轮转失败: {e}");
    }
    let config = ConfigBuilder::new().set_time_format_rfc3339().build();
    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = vec![TermLogger::new(
        LevelFilter::Info,
        config.clone(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )];
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        Ok(file) => {
            loggers.push(WriteLogger::new(LevelFilter::Info, config, file));
            eprintln!("[logger] file logger ready path={}", log_file.display());
        }
        Err(e) => {
            eprintln!(
                "[logger] ERROR open log file failed path={}: {e}",
                log_file.display()
            );
        }
    }
    let _ = CombinedLogger::init(loggers);
}
fn rotate_log_if_too_large(path: &std::path::Path) -> std::io::Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= LOG_ROTATE_LIMIT_BYTES {
        return Ok(());
    }
    let archive = path.with_file_name("openless.log.1");
    match std::fs::remove_file(&archive) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::rename(path, archive)
}
pub fn log_dir_path() -> std::path::PathBuf {
    {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home)
                .join("Library")
                .join("Logs")
                .join("OpenLess");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("OpenLess")
                .join("logs");
        }
    }
    std::env::temp_dir().join("OpenLess")
}
pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    activate_window_mode(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    activate_app(app);
}
/// 把听写 CLI intent 路由到共享 Core。首次启动与 single-instance 回调共用。
/// 异步动作通过 Tauri runtime spawn，不阻塞回调线程。
fn dispatch_cli_intent<R: Runtime>(app: &AppHandle<R>, intent: cli::CliIntent) {
    match intent {
        cli::CliIntent::ToggleDictation => {
            let backend = app
                .try_state::<Arc<openless_core::OpenLessBackend>>()
                .map(|state| Arc::clone(&*state));
            let Some(backend) = backend else {
                log::warn!("[cli] core backend not yet managed; dropping intent={intent:?}");
                return;
            };
            tauri::async_runtime::spawn(async move {
                log::info!("[cli] dispatching intent={intent:?} to core backend");
                if !backend.snapshot().running {
                    if let Err(error) = backend.start().await {
                        log::warn!("[cli] core backend start failed: {error}");
                        return;
                    }
                }
                if let Err(error) = backend.dispatch_cli_intent(intent).await {
                    // Cancelled 是空录音的静默结束，不是派发失败。
                    if error.code == openless_core::BackendErrorCode::Cancelled {
                        log::info!("[cli] core intent ended without output: {error}");
                    } else {
                        log::warn!("[cli] core intent failed: {error}");
                    }
                }
            });
        }
        cli::CliIntent::CancelDictation => {
            let coordinator = app
                .try_state::<Arc<coordinator::Coordinator>>()
                .map(|state| Arc::clone(&*state));
            let Some(coordinator) = coordinator else {
                log::warn!("[cli] coordinator not yet managed; dropping cancel intent");
                return;
            };
            tauri::async_runtime::spawn(async move {
                if let Err(error) = coordinator.cancel_dictation_from_cli().await {
                    log::warn!("[cli] cancel failed: {error}");
                }
            });
        }
    }
}
pub(crate) fn request_microphone_from_foreground<R: Runtime>(
    app: &AppHandle<R>,
) -> permissions::PermissionStatus {
    show_main_window(app);
    wait_for_app_activation(app);
    permissions::request_microphone()
}
fn hide_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    activate_menu_bar_mode(app);
}
fn activate_window_mode<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let _ = app.set_dock_visibility(true);
    let _ = app.show();
}
fn activate_menu_bar_mode<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    let _ = app.set_dock_visibility(false);
}
fn activate_app<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.run_on_main_thread(|| {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        unsafe {
            let Some(cls) = AnyClass::get("NSApplication") else {
                return;
            };
            let ns_app: *mut AnyObject = msg_send![cls, sharedApplication];
            if !ns_app.is_null() {
                let _: () = msg_send![ns_app, activateIgnoringOtherApps : Bool::YES];
            }
        }
    });
}
/// 展示胶囊后调用：若 OpenLess 已是前台 app，用 makeKeyWindow 还原主窗口焦点。
/// 不调 NSApp.activate，不抢其他 app 焦点，符合 CLAUDE.md 约束。
pub(crate) fn restore_main_window_key_if_active<R: Runtime>(app: &AppHandle<R>) {
    let main = app.get_webview_window("main");
    let _ = app.run_on_main_thread(move || {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        unsafe {
            let Some(cls) = AnyClass::get("NSApplication") else {
                return;
            };
            let ns_app: *mut AnyObject = msg_send![cls, sharedApplication];
            if ns_app.is_null() {
                return;
            }
            let is_active: Bool = msg_send![ns_app, isActive];
            if !is_active.as_bool() {
                return;
            }
            let Some(main) = main else {
                return;
            };
            match main.ns_window() {
                Ok(handle) => {
                    let main_win = handle as *mut AnyObject;
                    if !main_win.is_null() {
                        let _: () = msg_send![main_win, makeKeyWindow];
                    }
                }
                Err(e) => {
                    log::warn!("[main] ns_window unavailable for key restore: {e}")
                }
            };
        }
    });
}
fn wait_for_app_activation<R: Runtime>(app: &AppHandle<R>) {
    let (tx, rx) = mpsc::channel();
    let _ = app.run_on_main_thread(move || {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        unsafe {
            let Some(cls) = AnyClass::get("NSApplication") else {
                let _ = tx.send(());
                return;
            };
            let ns_app: *mut AnyObject = msg_send![cls, sharedApplication];
            if !ns_app.is_null() {
                let _: () = msg_send![ns_app, activateIgnoringOtherApps : Bool::YES];
            }
        }
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(Duration::from_millis(800));
    std::thread::sleep(Duration::from_millis(150));
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicalMonitorFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
fn logical_monitor_frame(
    physical_x: i32,
    physical_y: i32,
    physical_width: u32,
    physical_height: u32,
    scale: f64,
) -> LogicalMonitorFrame {
    let scale = scale.max(0.1);
    LogicalMonitorFrame {
        x: physical_x as f64 / scale,
        y: physical_y as f64 / scale,
        width: physical_width as f64 / scale,
        height: physical_height as f64 / scale,
    }
}
fn bottom_visual_position(
    frame: LogicalMonitorFrame,
    window_width: f64,
    visual_height: f64,
    bottom_padding: f64,
    bottom_inset: f64,
) -> (f64, f64) {
    let x = frame.x + ((frame.width - window_width) / 2.0).max(0.0);
    let y = frame.y + (frame.height - visual_height - bottom_padding - bottom_inset).max(0.0);
    (x, y)
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn frame_contains_point(frame: LogicalMonitorFrame, x: f64, y: f64) -> bool {
    x >= frame.x && x < frame.x + frame.width && y >= frame.y && y < frame.y + frame.height
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn frame_distance_to_point_squared(frame: LogicalMonitorFrame, x: f64, y: f64) -> f64 {
    let nearest_x = x.clamp(frame.x, frame.x + frame.width);
    let nearest_y = y.clamp(frame.y, frame.y + frame.height);
    let dx = x - nearest_x;
    let dy = y - nearest_y;
    dx * dx + dy * dy
}
/// 胶囊目标显示器快照：物理矩形 + DPI 缩放。
///
/// macOS 下由当前 focused input / caret 所在位置映射而来，供实际定位与
/// capsule layout cache 共用。用 Tauri monitor 的物理坐标作为稳定 key；
/// 真正 set_position 前再转成逻辑坐标，避免 Retina 下窗口尺寸翻倍。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CapsuleTargetMonitor {
    pub(crate) physical_x: i32,
    pub(crate) physical_y: i32,
    pub(crate) physical_width: u32,
    pub(crate) physical_height: u32,
    pub(crate) scale: f64,
}
impl CapsuleTargetMonitor {
    fn logical_frame(self) -> LogicalMonitorFrame {
        logical_monitor_frame(
            self.physical_x,
            self.physical_y,
            self.physical_width,
            self.physical_height,
            self.scale,
        )
    }
}
/// macOS：决定胶囊应该摆到哪块显示器。
///
/// 跟随**鼠标光标所在的屏**——这是用户的操作/视线焦点，也是唯一始终可用、
/// 无需任何权限的信号，多显示器 + 多 Space 下都能稳定命中。光标取不到时
/// （理论上不会）才退回 AX focused-input/caret 位置。
///
/// 关键：不能用 capsule window 自己的 current_monitor——窗口隐藏时它仍停留在
/// 上一次出现的屏，多屏会被缓存误判为“不需要移动”，把胶囊锁死在第一块屏。
/// 选屏时先找包含该点的屏；点短暂落在所有屏外则退到最近的屏，避免虚拟桌面
/// 负坐标 / 屏幕排列边缘导致完全不显示。定位与 layout 去重缓存共用本函数，
/// 二者看的必须是同一块屏。
pub(crate) fn capsule_target_monitor<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> Option<CapsuleTargetMonitor> {
    let (x, y) = macos_mouse_cursor_point().or_else(macos_focused_input_anchor_point)?;
    monitor_for_anchor_point(window, x, y)
}
/// 在 Tauri 的 monitor 坐标系里，选出包含逻辑坐标点 `(x, y)` 的显示器；
/// 点落在所有屏之外时退到最近的屏。坐标系同 AX / CGEvent 的全局显示空间
/// （左上原点，points）。
fn monitor_for_anchor_point<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    x: f64,
    y: f64,
) -> Option<CapsuleTargetMonitor> {
    let monitors = window.available_monitors().ok()?;
    pick_monitor_for_anchor_point(
        monitors.into_iter().map(|monitor| CapsuleTargetMonitor {
            physical_x: monitor.position().x,
            physical_y: monitor.position().y,
            physical_width: monitor.size().width,
            physical_height: monitor.size().height,
            scale: monitor.scale_factor(),
        }),
        x,
        y,
    )
}
/// 选屏本身的纯逻辑（不碰 Tauri / AppKit，便于单测多屏排列）：先返回包含该点的屏，
/// 都不包含时退到距离最近的屏。
fn pick_monitor_for_anchor_point(
    monitors: impl IntoIterator<Item = CapsuleTargetMonitor>,
    x: f64,
    y: f64,
) -> Option<CapsuleTargetMonitor> {
    let mut nearest: Option<(f64, CapsuleTargetMonitor)> = None;
    for target in monitors {
        let frame = target.logical_frame();
        if frame_contains_point(frame, x, y) {
            return Some(target);
        }
        let distance = frame_distance_to_point_squared(frame, x, y);
        match nearest {
            Some((best, _)) if best <= distance => {}
            _ => nearest = Some((distance, target)),
        }
    }
    nearest.map(|(_, target)| target)
}
fn macos_mouse_cursor_point() -> Option<(f64, f64)> {
    macos_capsule_ax::mouse_cursor_point()
}
fn macos_focused_input_anchor_point() -> Option<(f64, f64)> {
    macos_capsule_ax::focused_input_anchor_point()
}
mod macos_capsule_ax {
    use std::ffi::{c_void, CStr};
    use std::os::raw::c_char;
    #[repr(C)]
    struct OpaqueAxRef(c_void);
    type AxUiElementRef = *mut OpaqueAxRef;
    type CFStringRef = *const c_void;
    type CFTypeRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type AxError = i32;
    type AxValueRef = *const c_void;
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    const AX_ERROR_SUCCESS: AxError = 0;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_AX_VALUE_CG_POINT_TYPE: i32 = 1;
    const K_AX_VALUE_CG_SIZE_TYPE: i32 = 2;
    const K_AX_VALUE_CG_RECT_TYPE: i32 = 3;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AxUiElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AxUiElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AxError;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: AxUiElementRef,
            parameterized_attribute: CFStringRef,
            parameter: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> AxError;
        fn AXValueGetValue(value: AxValueRef, value_type: i32, out: *mut c_void) -> u8;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFStringCreateWithCString(
            allocator: CFAllocatorRef,
            cstr: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
    }
    pub(super) fn focused_input_anchor_point() -> Option<(f64, f64)> {
        unsafe {
            let focused = focused_element()?;
            let rect = caret_rect(focused).or_else(|| element_rect(focused));
            CFRelease(focused as CFTypeRef);
            let rect = rect?;
            let width = rect.size.width.max(1.0);
            let height = rect.size.height.max(1.0);
            Some((rect.origin.x + width / 2.0, rect.origin.y + height / 2.0))
        }
    }
    type CGEventRef = *const c_void;
    type CGEventSourceRef = *const c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
        fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    }
    /// 当前鼠标光标在「全局显示坐标系」(左上原点，points) 的位置。该坐标系与 AX
    /// caret 完全一致，可直接拿去和 `logical_frame` 比较选屏。`CGEventGetLocation`
    /// 始终可用、不需要任何权限，所以作为胶囊跟随屏幕的首选信号。
    pub(super) fn mouse_cursor_point() -> Option<(f64, f64)> {
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            if event.is_null() {
                return None;
            }
            let point = CGEventGetLocation(event);
            CFRelease(event as CFTypeRef);
            Some((point.x, point.y))
        }
    }
    unsafe fn cfstring_from_static(bytes_with_nul: &[u8]) -> Option<CFStringRef> {
        let cstr = CStr::from_bytes_with_nul(bytes_with_nul).ok()?;
        let s =
            CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8);
        if s.is_null() {
            None
        } else {
            Some(s)
        }
    }
    unsafe fn focused_element() -> Option<AxUiElementRef> {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        let Some(focused_attr) = cfstring_from_static(b"AXFocusedUIElement\0") else {
            CFRelease(system as CFTypeRef);
            return None;
        };
        let mut focused: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(system, focused_attr, &mut focused);
        CFRelease(system as CFTypeRef);
        CFRelease(focused_attr);
        if err != AX_ERROR_SUCCESS || focused.is_null() {
            None
        } else {
            Some(focused as AxUiElementRef)
        }
    }
    unsafe fn caret_rect(focused: AxUiElementRef) -> Option<CGRect> {
        let range_attr = cfstring_from_static(b"AXSelectedTextRange\0")?;
        let Some(bounds_attr) = cfstring_from_static(b"AXBoundsForRange\0") else {
            CFRelease(range_attr);
            return None;
        };
        let mut range_value: CFTypeRef = std::ptr::null();
        let range_err = AXUIElementCopyAttributeValue(focused, range_attr, &mut range_value);
        CFRelease(range_attr);
        if range_err != AX_ERROR_SUCCESS || range_value.is_null() {
            CFRelease(bounds_attr);
            return None;
        }
        let mut bounds_value: CFTypeRef = std::ptr::null();
        let bounds_err = AXUIElementCopyParameterizedAttributeValue(
            focused,
            bounds_attr,
            range_value,
            &mut bounds_value,
        );
        CFRelease(bounds_attr);
        CFRelease(range_value);
        if bounds_err != AX_ERROR_SUCCESS || bounds_value.is_null() {
            return None;
        }
        let mut rect = CGRect::default();
        let ok = AXValueGetValue(
            bounds_value as AxValueRef,
            K_AX_VALUE_CG_RECT_TYPE,
            &mut rect as *mut _ as *mut c_void,
        );
        CFRelease(bounds_value);
        (ok != 0).then_some(rect)
    }
    unsafe fn element_rect(focused: AxUiElementRef) -> Option<CGRect> {
        let position_attr = cfstring_from_static(b"AXPosition\0")?;
        let Some(size_attr) = cfstring_from_static(b"AXSize\0") else {
            CFRelease(position_attr);
            return None;
        };
        let mut position_value: CFTypeRef = std::ptr::null();
        let position_err =
            AXUIElementCopyAttributeValue(focused, position_attr, &mut position_value);
        CFRelease(position_attr);
        if position_err != AX_ERROR_SUCCESS || position_value.is_null() {
            CFRelease(size_attr);
            return None;
        }
        let mut point = CGPoint::default();
        let point_ok = AXValueGetValue(
            position_value as AxValueRef,
            K_AX_VALUE_CG_POINT_TYPE,
            &mut point as *mut _ as *mut c_void,
        );
        CFRelease(position_value);
        if point_ok == 0 {
            CFRelease(size_attr);
            return None;
        }
        let mut size_value: CFTypeRef = std::ptr::null();
        let size_err = AXUIElementCopyAttributeValue(focused, size_attr, &mut size_value);
        CFRelease(size_attr);
        if size_err != AX_ERROR_SUCCESS || size_value.is_null() {
            return Some(CGRect {
                origin: point,
                size: CGSize {
                    width: 1.0,
                    height: 1.0,
                },
            });
        }
        let mut size = CGSize::default();
        let size_ok = AXValueGetValue(
            size_value as AxValueRef,
            K_AX_VALUE_CG_SIZE_TYPE,
            &mut size as *mut _ as *mut c_void,
        );
        CFRelease(size_value);
        if size_ok == 0 {
            return Some(CGRect {
                origin: point,
                size: CGSize {
                    width: 1.0,
                    height: 1.0,
                },
            });
        }
        Some(CGRect {
            origin: point,
            size,
        })
    }
}
/// 把窗口左上角 `(x, y)`（同 area 同坐标系，physical px）夹到给定矩形内，
/// **保证整窗（含自身 w×h）落在 area 内可见**。area 为工作区时即可避开任务栏。
///
/// 纯函数，无 Win32 依赖，便于单测多显示器 / 负原点 / 异常 DPI 输入。issue #470：
/// 此前 Windows 分支只夹上边（`y.max(mon.top)`），左/右/下未夹，多屏负坐标下胶囊
/// 可能被算到屏外却无任何观测。这里四边都夹。
///
/// area 比窗口还小时（`area_right - w < area_left`），`max_x` 退化为 `area_left`，
/// `clamp` 把左上角收回 area 左上角，保证至少左上角可见、不溢出为负超界。
#[allow(dead_code)]
fn clamp_to_monitor(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    area_left: i32,
    area_top: i32,
    area_right: i32,
    area_bottom: i32,
) -> (i32, i32) {
    let max_x = area_right.saturating_sub(w).max(area_left);
    let max_y = area_bottom.saturating_sub(h).max(area_top);
    let clamped_x = x.clamp(area_left, max_x);
    let clamped_y = y.clamp(area_top, max_y);
    (clamped_x, clamped_y)
}

/// 把 capsule 窗口移到屏幕底部居中，与 Swift `CapsuleWindowController.repositionToBottomCenter` 同效。
/// 留 80pt 给 macOS Dock；Windows 任务栏一般在底部 48pt 以内，整体也合适。
pub(crate) fn position_capsule_bottom_center<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> tauri::Result<()> {
    let bounds = capsule_window_bounds();
    {
        if let Some(mon) = capsule_target_monitor(window) {
            window.set_size(LogicalSize::new(bounds.width, bounds.height))?;
            let frame = mon.logical_frame();
            let (x, y) = bottom_visual_position(
                frame,
                bounds.width,
                capsule_visual_height(),
                80.0,
                bounds.bottom_inset,
            );
            log::debug!(
                "[capsule] mac position: mon=({},{}) size=({}x{}) scale={:.2} -> logical=({:.1},{:.1})",
                mon.physical_x, mon.physical_y, mon.physical_width, mon.physical_height,
                mon.scale, x, y
            );
            window.set_position(LogicalPosition::new(x, y))?;
            return Ok(());
        }
    }
    let monitor = match window.current_monitor()? {
        Some(m) => m,
        None => return Ok(()),
    };
    window.set_size(LogicalSize::new(bounds.width, bounds.height))?;
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let pos = monitor.position();
    let frame = logical_monitor_frame(pos.x, pos.y, size.width, size.height, scale);
    let (x, y) = bottom_visual_position(
        frame,
        bounds.width,
        capsule_visual_height(),
        80.0,
        bounds.bottom_inset,
    );
    window.set_position(LogicalPosition::new(x, y))?;
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct CapsuleWindowBounds {
    width: f64,
    height: f64,
    bottom_inset: f64,
}
fn capsule_window_bounds() -> CapsuleWindowBounds {
    CapsuleWindowBounds {
        width: 460.0,
        height: 180.0,
        bottom_inset: 0.0,
    }
}
fn capsule_visual_height() -> f64 {
    140.0
}
#[cfg(test)]
mod tests {
    use super::{
        bottom_visual_position, capsule_visual_height, capsule_window_bounds, clamp_to_monitor,
        frame_contains_point, frame_distance_to_point_squared, logical_monitor_frame,
        rotate_log_if_too_large, LogicalMonitorFrame, TrayLabels, LOG_ROTATE_LIMIT_BYTES,
    };
    use super::{pick_monitor_for_anchor_point, CapsuleTargetMonitor};
    use std::io::Write;
    #[test]
    fn tray_labels_follow_locale() {
        let english = TrayLabels::for_locale("en");
        assert_eq!(english.toggle, "Show main window");
        assert_eq!(english.microphone, "Select microphone");
        assert_eq!(english.default_microphone, "System default microphone");
        assert_eq!(english.quit, "Quit OpenLess");
        assert_eq!(
            english.default_device_label("USB microphone"),
            "USB microphone (System default)"
        );
        assert_eq!(TrayLabels::for_locale("unknown").toggle, "显示主窗口");
    }
    #[test]
    fn capsule_window_bounds_match_voice_orb_stage() {
        let bounds = capsule_window_bounds();
        assert_eq!(
            (bounds.width, bounds.height, bounds.bottom_inset),
            (460.0, 180.0, 0.0)
        );
    }
    #[test]
    fn capsule_visual_height_sinks_stage_toward_dock() {
        assert_eq!(capsule_visual_height(), 140.0);
    }
    #[test]
    fn logical_monitor_frame_preserves_negative_origin() {
        let frame = logical_monitor_frame(-2560, 720, 5120, 2880, 2.0);
        assert_eq!(
            frame,
            LogicalMonitorFrame {
                x: -1280.0,
                y: 360.0,
                width: 2560.0,
                height: 1440.0,
            }
        );
    }
    #[test]
    fn monitor_frame_contains_points_with_negative_origins() {
        let frame = LogicalMonitorFrame {
            x: -1280.0,
            y: 360.0,
            width: 1280.0,
            height: 720.0,
        };
        assert!(frame_contains_point(frame, -640.0, 720.0));
        assert!(!frame_contains_point(frame, 10.0, 720.0));
        assert!(!frame_contains_point(frame, -640.0, 1080.0));
    }
    #[test]
    fn monitor_frame_distance_is_zero_inside_and_grows_outside() {
        let frame = LogicalMonitorFrame {
            x: 0.0,
            y: -900.0,
            width: 1440.0,
            height: 900.0,
        };
        assert_eq!(frame_distance_to_point_squared(frame, 100.0, -100.0), 0.0);
        assert_eq!(frame_distance_to_point_squared(frame, 100.0, 20.0), 400.0);
        assert_eq!(frame_distance_to_point_squared(frame, -10.0, -910.0), 200.0);
    }
    /// 内置 Retina 屏在左、外接 1x 屏在右的典型双屏排列。物理坐标按每块屏
    /// 自己的缩放表示，因此 1x 外接屏从内屏的逻辑右边缘 x=1512 开始。
    fn built_in_and_external_monitors() -> [CapsuleTargetMonitor; 2] {
        [
            CapsuleTargetMonitor {
                physical_x: 0,
                physical_y: 0,
                physical_width: 3024,
                physical_height: 1964,
                scale: 2.0,
            },
            CapsuleTargetMonitor {
                physical_x: 1512,
                physical_y: 0,
                physical_width: 2560,
                physical_height: 1440,
                scale: 1.0,
            },
        ]
    }
    /// 浮窗（QA / Less Computer 面板 / glow 描边）跟的是光标所在屏，不是窗口自己
    /// 上一次停留的屏 —— 这条一旦回退，多显示器下浮窗会被钉死在主屏。
    #[test]
    fn anchor_point_selects_the_monitor_under_the_cursor() {
        let monitors = built_in_and_external_monitors();
        let on_external = pick_monitor_for_anchor_point(monitors, 2000.0, 600.0)
            .expect("external monitor should win when the cursor is on it");
        assert_eq!(on_external, monitors[1]);
        let on_built_in = pick_monitor_for_anchor_point(monitors, 700.0, 600.0)
            .expect("built-in monitor should win when the cursor is on it");
        assert_eq!(on_built_in, monitors[0]);
    }
    /// 光标短暂落在两屏之间的空隙（排列错位 / 屏幕刚拔掉）时退到最近的屏，
    /// 而不是返回 None 让浮窗留在原地。
    #[test]
    fn anchor_point_outside_every_monitor_falls_back_to_the_nearest() {
        let monitors = built_in_and_external_monitors();
        let picked = pick_monitor_for_anchor_point(monitors, -100.0, 600.0)
            .expect("nearest monitor should be returned for an off-screen point");
        assert_eq!(picked, monitors[0]);
    }
    #[test]
    fn anchor_point_without_any_monitor_yields_none() {
        assert!(pick_monitor_for_anchor_point(Vec::new(), 0.0, 0.0).is_none());
    }
    #[test]
    fn bottom_visual_position_keeps_capsule_on_upper_monitor() {
        let frame = LogicalMonitorFrame {
            x: 0.0,
            y: -900.0,
            width: 1440.0,
            height: 900.0,
        };
        let pos = bottom_visual_position(frame, 220.0, 96.0, 80.0, 0.0);
        assert_eq!(pos, (610.0, -176.0));
    }
    #[test]
    fn clamp_to_monitor_leaves_on_screen_position_untouched() {
        let (x, y) = clamp_to_monitor(800, 900, 264, 126, 0, 0, 1920, 1040);
        assert_eq!((x, y), (800, 900));
    }
    #[test]
    fn clamp_to_monitor_pulls_back_off_screen_right_and_bottom() {
        let (x, y) = clamp_to_monitor(2000, 1200, 264, 126, 0, 0, 1920, 1040);
        assert_eq!((x, y), (1920 - 264, 1040 - 126));
        assert!(x + 264 <= 1920);
        assert!(y + 126 <= 1040);
    }
    #[test]
    fn clamp_to_monitor_pushes_into_negative_origin_left_monitor() {
        let (x, y) = clamp_to_monitor(-3000, -100, 294, 138, -2560, 0, 0, 1440);
        assert_eq!(x, -2560);
        assert_eq!(y, 0);
        assert!(x >= -2560 && x + 294 <= 0);
        assert!(y >= 0 && y + 138 <= 1440);
    }
    #[test]
    fn clamp_to_monitor_respects_work_area_above_taskbar() {
        let (_x, y) = clamp_to_monitor(800, 1030, 264, 126, 0, 0, 1920, 1040);
        assert_eq!(y, 1040 - 126);
        assert!(y + 126 <= 1040);
    }
    #[test]
    fn clamp_to_monitor_degrades_gracefully_when_window_wider_than_area() {
        let (x, y) = clamp_to_monitor(500, 500, 800, 600, 0, 0, 400, 300);
        assert_eq!((x, y), (0, 0));
    }
    #[test]
    fn oversized_log_rotates_to_single_archive() {
        let dir = std::env::temp_dir().join(format!("openless-log-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("openless.log");
        let archive = dir.join("openless.log.1");
        {
            let mut file = std::fs::File::create(&log).unwrap();
            file.set_len(LOG_ROTATE_LIMIT_BYTES + 1).unwrap();
            file.write_all(b"x").unwrap();
        }
        std::fs::write(&archive, b"old").unwrap();
        rotate_log_if_too_large(&log).unwrap();
        assert!(!log.exists());
        assert!(archive.exists());
        assert!(std::fs::metadata(&archive).unwrap().len() > LOG_ROTATE_LIMIT_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn small_log_does_not_rotate() {
        let dir = std::env::temp_dir().join(format!("openless-log-small-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("openless.log");
        let archive = dir.join("openless.log.1");
        std::fs::write(&log, b"small").unwrap();
        rotate_log_if_too_large(&log).unwrap();
        assert!(log.exists());
        assert!(!archive.exists());
        assert_eq!(std::fs::read(&log).unwrap(), b"small");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn missing_log_does_not_rotate() {
        let dir = std::env::temp_dir().join(format!("openless-log-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("openless.log");
        let archive = dir.join("openless.log.1");
        rotate_log_if_too_large(&log).unwrap();
        assert!(!log.exists());
        assert!(!archive.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
