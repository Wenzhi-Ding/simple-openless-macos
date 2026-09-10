//! 系统权限请求 / 检查（macOS / Windows）。
//!
//! 与 Swift `Sources/OpenLessHotkey/AccessibilityPermission.swift` +
//! `Sources/OpenLessRecorder/MicrophonePermission.swift` 同源。
//!
//! - macOS Accessibility：`AXIsProcessTrusted` 检查；
//!   `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})` 弹系统授权框。
//! - macOS Microphone：`AVAudioApplication.shared.recordPermission` + requestRecordPermission。
//! - Windows：cpal 不需要 Accessibility 等价权限；麦克风首次使用时 Win10+ 弹一次系统提示。
//!   没有可用输入设备时返回 `NoDevice`（区别于权限被拒的 `Denied`），避免把
//!   “没插麦克风”误报成“未授权麦克风”。详见 issue #779。
use serde::Serialize;
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Restricted,
    /// 当前平台不需要这个权限（如 Windows 上的 Accessibility）。
    NotApplicable,
    /// 当前没有可用的麦克风输入设备（不是权限问题）。
    NoDevice,
}
/// 错误字符串是否暗示“当前没有可用输入设备”（区别于权限被拒）。
/// cpal 在不同平台返回的文案不统一，靠关键字粗判。
pub(crate) fn is_no_device_error(lower: &str) -> bool {
    [
        "no default input device",
        "no default input",
        "no input device",
        "no device",
        "device not found",
        "device not available",
        "not connected",
        "unplugged",
        "disconnected",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}
mod platform {
    use super::PermissionStatus;
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFBooleanTrue: *const c_void;
    }
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *const c_void;
    }
    #[link(name = "AVFAudio", kind = "framework")]
    extern "C" {}
    pub fn check_accessibility() -> PermissionStatus {
        let status = unsafe {
            if AXIsProcessTrusted() {
                PermissionStatus::Granted
            } else {
                PermissionStatus::Denied
            }
        };
        log::info!("[permissions] name=accessibility status={status:?} source=AXIsProcessTrusted");
        status
    }
    /// 弹 Accessibility 系统授权框（只在未授权时弹）。返回当前授权状态。
    pub fn request_accessibility() -> PermissionStatus {
        unsafe {
            let key = kAXTrustedCheckOptionPrompt;
            let value = kCFBooleanTrue;
            let keys: [*const c_void; 1] = [key];
            let values: [*const c_void; 1] = [value];
            let dict = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
            );
            let trusted = AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
            if trusted {
                PermissionStatus::Granted
            } else {
                PermissionStatus::Denied
            }
        }
    }
    pub fn check_microphone() -> PermissionStatus {
        if let Some(status) = check_microphone_via_avaudio_application() {
            return status;
        }
        check_microphone_via_avcapture_device()
    }
    pub fn request_microphone() -> PermissionStatus {
        if let Some(status) = request_microphone_via_avaudio_application() {
            return status;
        }
        request_microphone_via_avcapture_device()
    }
    fn check_microphone_via_avaudio_application() -> Option<PermissionStatus> {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let cls = AnyClass::get("AVAudioApplication")?;
        let shared: *mut AnyObject = unsafe { msg_send![cls, sharedInstance] };
        if shared.is_null() {
            log::warn!("[mic] AVAudioApplication sharedInstance returned null");
            return None;
        }
        let perm: i64 = unsafe { msg_send![shared, recordPermission] };
        let mapped = match perm {
            0x6772_6e74 => PermissionStatus::Granted,
            0x6465_6e79 => PermissionStatus::Denied,
            0x756e_6474 => PermissionStatus::NotDetermined,
            _ => PermissionStatus::NotDetermined,
        };
        log::info!(
            "[mic] AVAudioApplication.recordPermission raw=0x{:x} ({}) → {:?}",
            perm,
            perm,
            mapped
        );
        Some(mapped)
    }
    fn request_microphone_via_avaudio_application() -> Option<PermissionStatus> {
        use block2::RcBlock;
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, Bool};
        let cls = AnyClass::get("AVAudioApplication")?;
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |granted: Bool| {
            let _ = tx.send(granted.as_bool());
        });
        log::info!("[mic] requesting via AVAudioApplication.requestRecordPermission");
        unsafe {
            let _: () = msg_send![
                cls, requestRecordPermissionWithCompletionHandler : &* block
            ];
        }
        let mapped = match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(true) => PermissionStatus::Granted,
            Ok(false) => PermissionStatus::Denied,
            Err(err) => {
                log::warn!("[mic] AVAudioApplication request timeout/error: {err}");
                check_microphone_via_avaudio_application()
                    .unwrap_or(PermissionStatus::NotDetermined)
            }
        };
        log::info!(
            "[mic] AVAudioApplication.requestRecordPermission → {:?}",
            mapped
        );
        Some(mapped)
    }
    fn check_microphone_via_avcapture_device() -> PermissionStatus {
        use objc2::msg_send;
        use objc2::runtime::AnyClass;
        let cls = match AnyClass::get("AVCaptureDevice") {
            Some(c) => c,
            None => return PermissionStatus::NotDetermined,
        };
        let status: i64 =
            unsafe { msg_send![cls, authorizationStatusForMediaType : AVMediaTypeAudio] };
        let mapped = match status {
            3 => PermissionStatus::Granted,
            2 => PermissionStatus::Denied,
            1 => PermissionStatus::Restricted,
            0 => PermissionStatus::NotDetermined,
            _ => PermissionStatus::NotDetermined,
        };
        log::info!(
            "[mic] AVCaptureDevice.authStatus raw={} → {:?}",
            status,
            mapped
        );
        mapped
    }
    fn request_microphone_via_avcapture_device() -> PermissionStatus {
        use block2::RcBlock;
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, Bool};
        let cls = match AnyClass::get("AVCaptureDevice") {
            Some(c) => c,
            None => return PermissionStatus::NotDetermined,
        };
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |granted: Bool| {
            let _ = tx.send(granted.as_bool());
        });
        log::info!("[mic] requesting via AVCaptureDevice.requestAccessForMediaType");
        unsafe {
            let _: () = msg_send![
                cls, requestAccessForMediaType : AVMediaTypeAudio completionHandler : &*
                block
            ];
        }
        let mapped = match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(true) => PermissionStatus::Granted,
            Ok(false) => PermissionStatus::Denied,
            Err(err) => {
                log::warn!("[mic] AVCaptureDevice request timeout/error: {err}");
                check_microphone_via_avcapture_device()
            }
        };
        log::info!("[mic] AVCaptureDevice.requestAccess → {:?}", mapped);
        mapped
    }
}
pub use platform::{
    check_accessibility, check_microphone, request_accessibility, request_microphone,
};
#[cfg(test)]
mod tests {
    use super::is_no_device_error;
    #[test]
    fn no_device_errors_are_recognized() {
        for message in [
            "no default input device",
            "No input device is available",
            "device not found",
            "default device is not connected",
            "unplugged microphone",
        ] {
            assert!(
                is_no_device_error(&message.to_lowercase()),
                "should classify as no-device: {message}"
            );
        }
    }
    #[test]
    fn permission_errors_are_not_no_device() {
        for message in ["access denied", "permission denied", "not authorized"] {
            assert!(
                !is_no_device_error(&message.to_lowercase()),
                "should NOT classify as no-device: {message}"
            );
        }
    }
}
