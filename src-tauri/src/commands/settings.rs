use super::*;

#[tauri::command]
pub fn get_settings(core: CoreState<'_>) -> UserPreferences {
    core.get_preferences()
}

struct TauriSettingsRuntime<'a> {
    coord: &'a Coordinator,
}

impl<'a> TauriSettingsRuntime<'a> {
    fn new(coord: &'a Coordinator) -> Self {
        Self { coord }
    }

    fn platform_error(message: impl Into<String>) -> openless_core::BackendError {
        openless_core::BackendError::new(openless_core::BackendErrorCode::Platform, message)
    }

    fn apply_hotkeys(
        &self,
        change: &openless_core::SettingsValueChange<openless_core::HotkeyRuntimeTarget>,
    ) -> Result<(), openless_core::BackendError> {
        self.coord
            .apply_hotkey_runtime_change(change)
            .map_err(Self::platform_error)
    }
}

impl openless_core::SettingsRuntime for TauriSettingsRuntime<'_> {
    fn prepare(
        &self,
        plan: &openless_core::SettingsEffectPlan,
    ) -> Result<openless_core::SettingsEffectReceipt, openless_core::SettingsEffectFailure> {
        let mut receipt = openless_core::SettingsEffectReceipt::default();
        if let Some(_change) = &plan.active_asr_provider {
            // 纯本地:激活的 ASR provider 只写 preferences,不再同步凭据 vault。
            receipt
                .applied
                .push(openless_core::SettingsEffectKind::ActiveAsrProvider);
        }
        Ok(receipt)
    }

    fn commit(
        &self,
        plan: &openless_core::SettingsEffectPlan,
        receipt: &mut openless_core::SettingsEffectReceipt,
    ) -> Result<(), openless_core::SettingsEffectFailure> {
        let Some(change) = &plan.hotkeys else {
            return Ok(());
        };
        if !receipt
            .applied
            .contains(&openless_core::SettingsEffectKind::Hotkeys)
        {
            receipt
                .applied
                .push(openless_core::SettingsEffectKind::Hotkeys);
        }
        self.apply_hotkeys(change).map_err(|error| {
            openless_core::SettingsEffectFailure::after_side_effect(error, receipt.clone())
        })
    }

    fn restore(
        &self,
        plan: &openless_core::SettingsEffectPlan,
        receipt: &openless_core::SettingsEffectReceipt,
    ) -> Result<(), openless_core::BackendError> {
        let mut failures = Vec::new();
        for effect in receipt.applied.iter().rev() {
            let result = match effect {
                openless_core::SettingsEffectKind::Hotkeys => plan
                    .hotkeys
                    .as_ref()
                    .map(|change| {
                        let reverse = openless_core::SettingsValueChange {
                            previous: change.next.clone(),
                            next: change.previous.clone(),
                        };
                        self.apply_hotkeys(&reverse)
                    })
                    .unwrap_or(Ok(())),
                openless_core::SettingsEffectKind::ActiveAsrProvider => Ok(()),
            };
            if let Err(error) = result {
                failures.push(error.message);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Self::platform_error(format!(
                "failed to restore settings runtime: {}",
                failures.join("; ")
            )))
        }
    }
}

pub(crate) fn persist_settings(
    coord: &Coordinator,
    mut prefs: UserPreferences,
) -> Result<(), String> {
    // 设置文档写入前的最后一道归一:前端发来的整份 prefs 可能带旧的
    // provider/OCR/自动更新值,固定运行时字段一律以强制值为准。
    crate::persistence::enforce_fixed_preferences(&mut prefs);
    let _host_guard = coord.lock_settings_host();
    coord
        .backend()
        .update_settings(
            prefs,
            openless_core::SettingsUpdateOptions::SETTINGS_DOCUMENT,
            &TauriSettingsRuntime::new(coord),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn persist_strict_settings(
    coord: &Coordinator,
    mut prefs: UserPreferences,
) -> Result<(), String> {
    crate::persistence::enforce_fixed_preferences(&mut prefs);
    let _host_guard = coord.lock_settings_host();
    coord
        .backend()
        .update_settings(
            prefs,
            openless_core::SettingsUpdateOptions::STRICT,
            &TauriSettingsRuntime::new(coord),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_settings(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    tray_microphones: State<'_, TrayMicrophoneMenuState>,
    prefs: UserPreferences,
) -> Result<(), String> {
    // 广播给所有 webview。issue #205：QaPanel 跑在独立 webview，
    // 没有 HotkeySettingsContext，必须靠事件感知录音键变化，否则面板可见时
    // 用户改键会让浮窗里的 "{recordHotkey}" 文案一直停留在旧值。
    persist_settings(&*coord, prefs)?;
    let prefs = coord.backend().get_preferences();
    // 保存即同步胶囊样式原子：下一次录音的入场帧就携带新样式，不依赖 emit_capsule
    // 主线程闭包的 ~30Hz 同步（Windows 主线程拥塞时闭包延迟 → 整场显示旧样式）。
    // 前端也会通过 prefs:changed 广播收到新样式，录音中切换即时换肤。
    coord.sync_capsule_style_from_preferences();
    // refresh_tray_microphone_menu 内部会调用 NSStatusItem.set_menu，必须在主线程上跑。
    // set_settings 是异步 Tauri command，执行期间不在 macOS UI 主线程；从这里直接调
    // 会触发 macOS 主线程断言或在 dispatch 队列上死锁，导致整个 UI 无响应（用户改
    // 偏好后所有按键都没反应即此根因）。dispatch 到主线程后继续处理，异步任务不阻塞。
    let app_for_main = app.clone();
    let prefs_for_main = prefs.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(err) = crate::refresh_tray_microphone_menu(&app_for_main) {
            log::warn!("[tray] refresh microphone menu after settings save failed: {err}");
            let tray_state = app_for_main.state::<TrayMicrophoneMenuState>();
            sync_tray_microphone_selection(
                &tray_state.lock(),
                &prefs_for_main.microphone_device_name,
            );
        }
    });
    // 抑制 unused 警告：tray_microphones 现在改在闭包里通过 app.state 取，
    // 但函数签名保留 State 入参，以便 Tauri 在调用前注入。
    let _ = tray_microphones;
    Ok(())
}
