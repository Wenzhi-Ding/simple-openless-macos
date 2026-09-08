use super::*;

#[tauri::command]
pub fn get_settings(core: CoreState<'_>) -> UserPreferences {
    core.get_preferences()
}

#[tauri::command]
pub fn get_default_style_system_prompts() -> StyleSystemPrompts {
    StyleSystemPrompts::default()
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

    fn apply_windows_keyboard(
        &self,
        target: &openless_core::WindowsKeyboardRuntimeTarget,
    ) -> Result<(), openless_core::BackendError> {
        crate::windows_ime_profile::apply_windows_openless_keyboard_list(
            target.openless_language_profile_enabled,
        )
        .map_err(Self::platform_error)
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
        if let Some(change) = &plan.windows_keyboard {
            if let Err(error) = self.apply_windows_keyboard(&change.next) {
                return Err(openless_core::SettingsEffectFailure::after_side_effect(
                    error, receipt,
                ));
            }
            receipt
                .applied
                .push(openless_core::SettingsEffectKind::WindowsKeyboard);
        }
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
                openless_core::SettingsEffectKind::WindowsKeyboard => plan
                    .windows_keyboard
                    .as_ref()
                    .map(|change| self.apply_windows_keyboard(&change.previous))
                    .unwrap_or(Ok(())),
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
        .map(|outcome| {
            if outcome.reconciled_hotkey_count > 0 {
                log::warn!(
                    "[settings] 热键冲突已自动化解（调整 {} 项）后保存",
                    outcome.reconciled_hotkey_count
                );
            }
        })
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

#[cfg(not(mobile))]
#[tauri::command]
pub async fn set_settings(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    tray_microphones: State<'_, TrayMicrophoneMenuState>,
    mut prefs: UserPreferences,
) -> Result<(), String> {
    // 捕获旧值用于远程输入服务的 diff（persist 后端口/开关变化时启停/重启）。
    let remote_prev = coord.backend().get_preferences();
    let packs = coord
        .backend()
        .list_style_packs(&prefs.active_style_pack_id)
        .map_err(|e| e.to_string())?;
    sync_style_pack_preferences(&mut prefs, &packs);
    prefs.android_overlay_trigger = prefs.android_overlay_trigger.normalized();
    // 广播给所有 webview。issue #205：QaPanel 跑在独立 webview，
    // 没有 HotkeySettingsContext，必须靠事件感知录音键变化，否则面板可见时
    // 用户改键会让浮窗里的 "{recordHotkey}" 文案一直停留在旧值。
    persist_settings(&*coord, prefs)?;
    let prefs = coord.backend().get_preferences();
    // 保存即同步胶囊样式原子：下一次录音的入场帧就携带新样式，不依赖 emit_capsule
    // 主线程闭包的 ~30Hz 同步（Windows 主线程拥塞时闭包延迟 → 整场显示旧样式）。
    // 前端也会通过 prefs:changed 广播收到新样式，录音中切换即时换肤。
    coord.sync_capsule_style_from_preferences();
    #[cfg(target_os = "android")]
    coord.apply_android_overlay_settings_change(&remote_prev, &prefs);
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

#[cfg(mobile)]
#[tauri::command]
pub fn set_settings(coord: CoordinatorState<'_>, mut prefs: UserPreferences) -> Result<(), String> {
    let previous = coord.backend().get_preferences();
    let packs = coord
        .backend()
        .list_style_packs(&prefs.active_style_pack_id)
        .map_err(|e| e.to_string())?;
    sync_style_pack_preferences(&mut prefs, &packs);
    prefs.android_overlay_trigger = prefs.android_overlay_trigger.normalized();
    persist_settings(&*coord, prefs)?;
    let prefs = coord.backend().get_preferences();
    // 保存即同步胶囊样式原子（Android 通知胶囊 payload 同源，见 emit_capsule）。
    coord.sync_capsule_style_from_preferences();
    #[cfg(target_os = "android")]
    coord.apply_android_overlay_settings_change(&previous, &prefs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_save_preserves_current_style_preferences_before_write() {
        let packs = crate::types::builtin_style_packs();
        let current = UserPreferences {
            default_mode: PolishMode::Light,
            active_style_pack_id: builtin_style_pack_id(PolishMode::Light).to_string(),
            ..UserPreferences::default()
        };
        let mut stale_settings_payload = UserPreferences {
            default_mode: PolishMode::Formal,
            active_style_pack_id: builtin_style_pack_id(PolishMode::Formal).to_string(),
            ..UserPreferences::default()
        };

        stale_settings_payload.preserve_style_preferences_from(&current);
        sync_style_pack_preferences(&mut stale_settings_payload, &packs);

        assert_eq!(
            stale_settings_payload.active_style_pack_id,
            builtin_style_pack_id(PolishMode::Light)
        );
        assert_eq!(stale_settings_payload.default_mode, PolishMode::Light);
    }
}

// ─────────────────────────── release channel (Beta opt-in) ───────────────────────────
//
// 渠道偏好的写入路径跟 set_settings 复用 persist_settings：保持热键兜底归一化
// 跟其他 prefs 写入一致，且写完后 emit "prefs:changed"，让前端跨 webview 同步。
//
// 更新：plugin-updater 2.10.1 的 Builder 现在暴露 .endpoints() runtime API（CLAUDE.md
// 当年记的"不支持"已不成立）。本节配合 `app_check_update_with_channel` 命令实现
// Beta auto-update：Stable 渠道 → 走 tauri.conf 的默认 endpoints；Beta 渠道 →
// fetch_latest_beta_release 拿最新 prerelease tag → 拼成 -beta manifest URL →
// builder.endpoints(vec![url]).build().check()。Stable 用户绝对不会撞到 Beta 包
// （Beta tag 的 manifest 文件名带 `-beta` 后缀，跟 Stable manifest 在 GitHub
// Release assets 里物理分离）。






fn is_beta_release_tag(tag_name: &str) -> bool {
    if tag_name.ends_with("-beta-tauri") {
        return true;
    }

    let Some((version, beta_number)) = tag_name
        .strip_prefix('v')
        .and_then(|tag| tag.strip_suffix("-tauri"))
        .and_then(|tag| tag.split_once("-Beta."))
    else {
        return false;
    };

    if beta_number.is_empty() || !beta_number.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    let mut version_parts = version.split('.');
    (0..3).all(|_| {
        version_parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    }) && version_parts.next().is_none()
}

fn extract_between(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let end = haystack[start..].find(close)?;
    Some(haystack[start..start + end].to_string())
}

// ─────────────────────── Channel-aware updater check ────────────────────────
//
// 替换前端原来直接 import('@tauri-apps/plugin-updater').check() 的路径：
// - Stable 渠道：builder 不动 endpoints，沿用 tauri.conf 配的 stable manifest URL。
// - Beta 渠道：先 fetch_latest_beta_release 拿最新 prerelease tag，拼成 -beta manifest
//   URL（同时给一对 mirror + direct），再 builder.endpoints(vec![url])?.build()?.check()。
//
// 返回的 Metadata 形状与 plugin-updater 的 JS UpdateMetadata 完全一致（rid +
// currentVersion 等驼峰字段），前端可以直接 `new Update(metadata)` 复用 plugin
// 的 download / install / close 实现，无需我们自己写下载和签名校验。
//
// 物理隔离：Beta tag 推出来的 manifest 文件名带 `-beta` 后缀（参见 release-tauri.yml
// 第 382 行注释），跟 Stable 的 `latest-{tgt}-{arch}.json` 在 GitHub Release assets
// 里是分开的两份文件 —— 即使代码逻辑写错把 Beta URL 传给 Stable 用户，HTTP 也是
// 直接 404，绝不会拿到错档。

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateMetadata {
    #[cfg(not(mobile))]
    pub rid: tauri::ResourceId,
    #[cfg(mobile)]
    pub rid: u32,
    pub current_version: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
    /// 原始 manifest JSON——桌面 `new Update(metadata)` / Android 自定义安装路径共用。
    pub raw_json: serde_json::Value,
}




#[cfg(mobile)]
#[tauri::command]
pub async fn app_download_and_install_android_update(
    app: AppHandle,
    url: String,
    signature: String,
    version: String,
) -> Result<(), String> {
    // 安全：下载前校验 URL，防止 SSRF（如内网元数据接口、localhost 服务）。
    // 只允许已知的 GitHub 直链和 fastgit 镜像前缀。
    const DIRECT_BASE: &str = "https://github.com/Open-Less/openless";
    const MIRROR_BASE: &str = "https://fastgit.cc/https://github.com/Open-Less/openless";
    if !url.starts_with(DIRECT_BASE) && !url.starts_with(MIRROR_BASE) {
        return Err(format!("不信任的更新 URL，拒绝下载: {url}"));
    }
    #[cfg(target_os = "android")]
    {
        return crate::android::updater::download_and_install(app, url, signature, version).await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, url, signature, version);
        Err("应用内更新仅支持 Android".to_string())
    }
}