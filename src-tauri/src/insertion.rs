//! macOS 光标位置文本插入。
//!
//! 先写剪贴板，再用 CoreGraphics CGEvent post Cmd+V。
//! 目标应用可能延迟读取剪贴板，因此听写文本必须保留，不读取或恢复旧内容。
use crate::types::InsertStatus;
/// 把一段文字放进剪贴板。供落字失败兜底卡片的「复制」按钮使用。
///
/// 单独开这个入口而不是让前端调 `navigator.clipboard`：卡片浮在别的 app 上、按钮
/// 不抢焦点，未聚焦文档里那个 API 会抛 `Document is not focused`。
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if copy_to_clipboard(text) {
        Ok(())
    } else {
        Err("clipboard write failed".to_string())
    }
}
pub struct TextInserter;
impl TextInserter {
    pub fn new() -> Self {
        Self
    }
    /// 写入成功后才 post Cmd+V；post 成功不代表目标应用已经读取文本。
    pub fn insert(&self, text: &str) -> InsertStatus {
        insert_with(text, copy_to_clipboard, simulate_paste)
    }
}
impl Default for TextInserter {
    fn default() -> Self {
        Self::new()
    }
}
fn insert_with<W, P>(text: &str, write_clipboard: W, paste: P) -> InsertStatus
where
    W: FnOnce(&str) -> bool,
    P: FnOnce() -> Result<(), String>,
{
    if text.is_empty() {
        return InsertStatus::CopiedFallback;
    }
    if !write_clipboard(text) {
        return InsertStatus::Failed;
    }
    if let Err(err) = paste() {
        log::warn!("[insertion] simulated paste failed: {}", err);
        return InsertStatus::CopiedFallback;
    }
    InsertStatus::Inserted
}
fn copy_to_clipboard(text: &str) -> bool {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(err) => {
            log::error!("[insertion] clipboard init failed: {}", err);
            return false;
        }
    };
    if let Err(err) = clipboard.set_text(text.to_string()) {
        log::error!("[insertion] clipboard set_text failed: {}", err);
        return false;
    }
    true
}
fn simulate_macos_paste_with<P>(
    accessibility_granted: bool,
    secure_input_active: bool,
    post_cmd_v: P,
) -> Result<(), String>
where
    P: FnOnce() -> Result<(), String>,
{
    if !accessibility_granted {
        return Err("accessibility permission is not granted".into());
    }
    if secure_input_active {
        return Err("secure input is active".into());
    }
    post_cmd_v()
}
fn simulate_paste() -> Result<(), String> {
    simulate_macos_paste_with(
        matches!(
            crate::permissions::check_accessibility(),
            crate::permissions::PermissionStatus::Granted
        ),
        crate::unicode_keystroke::is_secure_input_enabled(),
        macos::post_cmd_v,
    )
}
mod macos {
    use std::ffi::c_void;
    #[repr(C)]
    struct OpaqueCGEvent(c_void);
    type CGEventRef = *mut OpaqueCGEvent;
    #[repr(C)]
    struct OpaqueCGEventSource(c_void);
    type CGEventSourceRef = *mut OpaqueCGEventSource;
    type CGEventTapLocation = u32;
    type CGEventSourceStateID = i32;
    type CGKeyCode = u16;
    type CGEventFlags = u64;
    const KCG_HID_EVENT_TAP: CGEventTapLocation = 0;
    const KCG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: CGEventSourceStateID = 1;
    const KCG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x00100000;
    /// US/ANSI 键盘上 "V" 的虚拟键码（kVK_ANSI_V）。
    const KEY_V: CGKeyCode = 9;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state_id: CGEventSourceStateID) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtual_key: CGKeyCode,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
        fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    /// 模拟 Cmd+V：构造 V 的 down/up 事件，加 Cmd flag 后依次 post 到 HID 事件流。
    pub fn post_cmd_v() -> Result<(), String> {
        unsafe {
            let source = CGEventSourceCreate(KCG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
            let down = CGEventCreateKeyboardEvent(source, KEY_V, true);
            let up = CGEventCreateKeyboardEvent(source, KEY_V, false);
            if down.is_null() || up.is_null() {
                if !source.is_null() {
                    CFRelease(source as *const c_void);
                }
                if !down.is_null() {
                    CFRelease(down as *const c_void);
                }
                if !up.is_null() {
                    CFRelease(up as *const c_void);
                }
                return Err("CGEventCreateKeyboardEvent returned null".into());
            }
            CGEventSetFlags(down, KCG_EVENT_FLAG_MASK_COMMAND);
            CGEventSetFlags(up, KCG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(KCG_HID_EVENT_TAP, down);
            CGEventPost(KCG_HID_EVENT_TAP, up);
            CFRelease(down as *const c_void);
            CFRelease(up as *const c_void);
            if !source.is_null() {
                CFRelease(source as *const c_void);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn failed_clipboard_write_never_posts_paste() {
        assert_eq!(
            insert_with("dictated text", |_| false, || panic!("must not post")),
            InsertStatus::Failed
        );
    }

    #[test]
    fn clipboard_write_precedes_paste() {
        let calls = RefCell::new(Vec::new());
        assert_eq!(
            insert_with(
                "dictated text",
                |text| {
                    assert_eq!(text, "dictated text");
                    calls.borrow_mut().push("write");
                    true
                },
                || {
                    calls.borrow_mut().push("post");
                    Ok(())
                },
            ),
            InsertStatus::Inserted
        );
        assert_eq!(*calls.borrow(), ["write", "post"]);
    }

    #[test]
    fn failed_paste_keeps_dictated_text_for_manual_paste() {
        let clipboard = RefCell::new(String::from("old text"));
        let status = insert_with(
            "dictated text",
            |text| {
                *clipboard.borrow_mut() = text.to_string();
                true
            },
            || {
                assert_eq!(*clipboard.borrow(), "dictated text");
                Err("post failed".into())
            },
        );
        assert_eq!(status, InsertStatus::CopiedFallback);
        assert_eq!(*clipboard.borrow(), "dictated text");
    }

    #[test]
    fn successful_paste_keeps_text_for_delayed_reader_without_restoring() {
        let clipboard = RefCell::new(String::from("old text"));
        let writes = RefCell::new(Vec::new());
        let status = insert_with(
            "dictated text",
            |text| {
                *clipboard.borrow_mut() = text.to_string();
                writes.borrow_mut().push(text.to_string());
                true
            },
            || Ok(()),
        );
        assert_eq!(status, InsertStatus::Inserted);
        std::thread::sleep(std::time::Duration::from_millis(1_000));
        assert_eq!(*clipboard.borrow(), "dictated text");
        assert_eq!(*writes.borrow(), ["dictated text"]);
    }

    #[test]
    fn empty_insertions_never_touch_clipboard_or_paste_path() {
        assert_eq!(
            insert_with("", |_| panic!("must not write"), || panic!("must not post")),
            InsertStatus::CopiedFallback
        );
        let inserter = TextInserter::new();
        assert_eq!(
            inserter.insert(""),
            InsertStatus::CopiedFallback
        );
    }
    #[test]
    fn macos_paste_preflight_rejects_missing_accessibility_before_posting() {
        let mut posted = false;
        let result = simulate_macos_paste_with(false, false, || {
            posted = true;
            Ok(())
        });
        assert_eq!(
            result.unwrap_err(),
            "accessibility permission is not granted"
        );
        assert!(!posted);
    }
    #[test]
    fn macos_paste_preflight_rejects_secure_input_before_posting() {
        let mut posted = false;
        let result = simulate_macos_paste_with(true, true, || {
            posted = true;
            Ok(())
        });
        assert_eq!(result.unwrap_err(), "secure input is active");
        assert!(!posted);
    }
    #[test]
    fn macos_paste_preflight_posts_when_guards_are_clear() {
        let mut posted = false;
        let result = simulate_macos_paste_with(true, false, || {
            posted = true;
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert!(posted);
    }
}
