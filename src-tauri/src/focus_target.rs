//! macOS foreground application identity and dictation insertion target.
use std::time::Duration;
#[derive(Clone, Copy, Debug)]
pub(crate) struct InsertionTarget {
    pid: i32,
}
impl InsertionTarget {
    pub(crate) fn capture() -> Option<Self> {
        current_front_app_pid().map(|pid| Self { pid })
    }
    pub(crate) fn reactivate(self) -> bool {
        activate_app_by_pid(self.pid);
        std::thread::sleep(Duration::from_millis(120));
        current_front_app_pid() == Some(self.pid)
    }
}
pub(crate) fn current_front_app_parts() -> (Option<String>, Option<String>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    unsafe {
        let Some(cls) = AnyClass::get("NSWorkspace") else {
            return (None, None);
        };
        let workspace: *mut AnyObject = msg_send![cls, sharedWorkspace];
        if workspace.is_null() {
            return (None, None);
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return (None, None);
        }
        let name: *mut AnyObject = msg_send![app, localizedName];
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        (ns_string(name), ns_string(bundle))
    }
}
pub(crate) fn bundle_id_for_pid(pid: i32) -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    unsafe {
        let cls = AnyClass::get("NSRunningApplication")?;
        let app: *mut AnyObject = msg_send![cls,runningApplicationWithProcessIdentifier:pid];
        if app.is_null() {
            return None;
        }
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        ns_string(bundle)
    }
}
pub(crate) fn current_front_app_pid() -> Option<i32> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    unsafe {
        let cls = AnyClass::get("NSWorkspace")?;
        let workspace: *mut AnyObject = msg_send![cls, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        (pid > 0).then_some(pid)
    }
}
fn activate_app_by_pid(pid: i32) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        let Some(cls) = AnyClass::get("NSRunningApplication") else {
            return;
        };
        let app: *mut objc2::runtime::AnyObject =
            msg_send![cls,runningApplicationWithProcessIdentifier:pid];
        if !app.is_null() {
            let _: () = msg_send![app,activateWithOptions:1u64];
        }
    }
}
unsafe fn ns_string(value: *mut objc2::runtime::AnyObject) -> Option<String> {
    use objc2::msg_send;
    if value.is_null() {
        return None;
    }
    let p: *const std::os::raw::c_char = unsafe { msg_send![value, UTF8String] };
    if p.is_null() {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    (!s.is_empty()).then_some(s)
}
