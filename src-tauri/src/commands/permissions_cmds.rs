use super::*;
#[tauri::command]
pub async fn get_platform_capabilities(
    core: CoreState<'_>,
) -> Result<crate::types::PlatformCapabilities, String> {
    Ok(core
        .services()
        .platform
        .capabilities()
        .await
        .unwrap_or_default())
}
#[tauri::command]
pub async fn check_accessibility_permission(
    core: CoreState<'_>,
) -> Result<PermissionStatus, String> {
    core.services()
        .platform
        .accessibility_permission()
        .await
        .map(|snapshot| map_core_permission(snapshot.accessibility))
        .map_err(|_| {
            log::warn!("[permissions] name=accessibility status=checkFailed source=core");
            "accessibility permission check failed".to_string()
        })
}
#[tauri::command]
pub async fn request_accessibility_permission(
    core: CoreState<'_>,
) -> Result<PermissionStatus, String> {
    if let Err(error) = core
        .services()
        .platform
        .request_accessibility_permission()
        .await
    {
        let _ = error;
        log::warn!("[permissions] name=accessibility status=checkFailed source=request");
        return Err("accessibility permission request failed".to_string());
    }
    check_accessibility_permission(core).await
}
#[tauri::command]
pub async fn check_microphone_permission(core: CoreState<'_>) -> Result<PermissionStatus, String> {
    core.services()
        .platform
        .microphone_permission()
        .await
        .map(|snapshot| map_core_permission(snapshot.microphone))
        .map_err(|_| {
            log::warn!("[permissions] name=microphone status=checkFailed source=core");
            "microphone permission check failed".to_string()
        })
}
#[tauri::command]
pub async fn request_microphone_permission(
    core: CoreState<'_>,
) -> Result<PermissionStatus, String> {
    if let Err(error) = core
        .services()
        .platform
        .request_microphone_permission()
        .await
    {
        let _ = error;
        log::warn!("[permissions] name=microphone status=checkFailed source=request");
        return Err("microphone permission request failed".to_string());
    }
    check_microphone_permission(core).await
}
fn map_core_permission(state: openless_core::PermissionState) -> PermissionStatus {
    match state {
        openless_core::PermissionState::Unknown => PermissionStatus::NotDetermined,
        openless_core::PermissionState::Granted => PermissionStatus::Granted,
        openless_core::PermissionState::Denied => PermissionStatus::Denied,
        openless_core::PermissionState::Restricted => PermissionStatus::Restricted,
        openless_core::PermissionState::NoDevice => PermissionStatus::NoDevice,
        openless_core::PermissionState::Unsupported => PermissionStatus::NotApplicable,
    }
}
/// 跳到 macOS 系统设置的指定隐私面板。pane: "accessibility" | "microphone".
#[tauri::command]
pub fn open_system_settings(pane: String) -> Result<(), String> {
    {
        let url = match pane.as_str() {
            "accessibility" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            "microphone" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            "screenCapture" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            _ => "x-apple.systempreferences:com.apple.preference.security?Privacy",
        };
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
/// 触发 macOS 系统弹"是否允许 OpenLess 访问麦克风"对话框。
/// 与 Swift `MicrophonePermission.request()` 同语义：只信系统权限回调，
/// 不用 cpal stream 成功与否伪造授权状态。
#[tauri::command]
pub async fn trigger_microphone_prompt(core: CoreState<'_>) -> Result<(), String> {
    let status = request_microphone_permission(core).await?;
    if matches!(
        status,
        PermissionStatus::Granted | PermissionStatus::NotApplicable
    ) {
        Ok(())
    } else {
        Err(format!("microphone permission is {status:?}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_permission_states_preserve_the_legacy_ipc_contract() {
        let cases = [
            (
                openless_core::PermissionState::Unknown,
                PermissionStatus::NotDetermined,
            ),
            (
                openless_core::PermissionState::Granted,
                PermissionStatus::Granted,
            ),
            (
                openless_core::PermissionState::Denied,
                PermissionStatus::Denied,
            ),
            (
                openless_core::PermissionState::Restricted,
                PermissionStatus::Restricted,
            ),
            (
                openless_core::PermissionState::NoDevice,
                PermissionStatus::NoDevice,
            ),
            (
                openless_core::PermissionState::Unsupported,
                PermissionStatus::NotApplicable,
            ),
        ];
        for (core, legacy) in cases {
            assert_eq!(map_core_permission(core), legacy);
        }
    }
}
#[tauri::command]
pub fn check_screen_capture_permission() -> PermissionStatus {
    {
        let status = if crate::screen_ocr::screen_capture_allowed() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::NotDetermined
        };
        log::info!(
            "[permissions] name=screenCapture status={status:?} source=CGPreflightScreenCaptureAccess"
        );
        status
    }
}
#[tauri::command]
pub fn request_screen_capture_permission() -> PermissionStatus {
    {
        if crate::screen_ocr::request_screen_capture_permission() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::NotDetermined
        }
    }
}
