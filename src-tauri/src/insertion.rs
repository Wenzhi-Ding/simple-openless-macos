//! 跨平台光标位置文本插入。
//!
//! 通用步骤：先写剪贴板（模拟失败时用户能手动粘贴）→ 模拟粘贴快捷键。
//! - macOS：用 CoreGraphics CGEvent 直接 post Cmd+V。
//! - Windows / Linux：用 enigo 按 `PasteShortcut` 模拟。
use crate::types::{InsertStatus, PasteShortcut};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(750);
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
    /// macOS 路径：保存原剪贴板 → 写转写文字 → post Cmd+V → 按需恢复原剪贴板。
    /// `_paste_shortcut` 在 macOS 不使用（固定 Cmd+V），仅为对齐跨平台签名。
    pub fn insert(
        &self,
        text: &str,
        restore_clipboard_after_paste: bool,
        _paste_shortcut: PasteShortcut,
    ) -> InsertStatus {
        if text.is_empty() {
            return InsertStatus::CopiedFallback;
        }
        let restore_plan = match copy_to_clipboard_with_restore_plan(text) {
            Ok(plan) => plan,
            Err(err) => {
                log::error!("[insertion] clipboard write failed: {}", err);
                return InsertStatus::Failed;
            }
        };
        if let Err(err) = simulate_paste() {
            log::warn!("[insertion] simulated paste failed: {}", err);
            return InsertStatus::CopiedFallback;
        }
        if restore_clipboard_after_paste {
            schedule_clipboard_restore(restore_plan);
        }
        insertion_success_status()
    }
    /// 只写剪贴板、不模拟粘贴。用于目标控件活跃状态无法验证时的兜底路径。
    pub fn copy_fallback(&self, text: &str) -> InsertStatus {
        if text.is_empty() {
            return InsertStatus::CopiedFallback;
        }
        if copy_to_clipboard(text) {
            InsertStatus::CopiedFallback
        } else {
            InsertStatus::Failed
        }
    }
}
impl Default for TextInserter {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Debug)]
struct ClipboardRestorePlan {
    inserted_text: String,
    previous_text: Option<String>,
}
#[derive(Debug, Clone)]
struct PendingClipboardRestore {
    latest_restore_id: u64,
    original_text: Option<String>,
}
static NEXT_CLIPBOARD_RESTORE_ID: AtomicU64 = AtomicU64::new(1);
static PENDING_CLIPBOARD_RESTORE: Lazy<Mutex<Option<PendingClipboardRestore>>> =
    Lazy::new(|| Mutex::new(None));
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
fn copy_to_clipboard_with_restore_plan(text: &str) -> Result<ClipboardRestorePlan, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let previous_text = match clipboard.get_text() {
        Ok(existing) => Some(existing),
        Err(err) => {
            log::warn!(
                "[insertion] clipboard get_text failed before overwrite: {}",
                err
            );
            None
        }
    };
    clipboard
        .set_text(text.to_string())
        .map_err(|e| e.to_string())?;
    Ok(ClipboardRestorePlan {
        inserted_text: text.to_string(),
        previous_text,
    })
}
fn schedule_clipboard_restore(plan: ClipboardRestorePlan) {
    let (restore_id, original_text) =
        remember_pending_clipboard_restore(plan.previous_text.clone());
    std::thread::spawn(move || {
        restore_clipboard_after_delay(plan, original_text, restore_id, CLIPBOARD_RESTORE_DELAY)
    });
}
fn remember_pending_clipboard_restore(previous_text: Option<String>) -> (u64, Option<String>) {
    let restore_id = NEXT_CLIPBOARD_RESTORE_ID.fetch_add(1, Ordering::SeqCst);
    let original_text = {
        let mut pending = PENDING_CLIPBOARD_RESTORE.lock();
        let original = pending
            .as_ref()
            .map(|batch| batch.original_text.clone())
            .unwrap_or(previous_text);
        *pending = Some(PendingClipboardRestore {
            latest_restore_id: restore_id,
            original_text: original.clone(),
        });
        original
    };
    (restore_id, original_text)
}
fn restore_clipboard_after_delay(
    plan: ClipboardRestorePlan,
    original_text: Option<String>,
    restore_id: u64,
    delay: Duration,
) {
    std::thread::sleep(delay);
    if !is_latest_clipboard_restore(restore_id) {
        return;
    }
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(err) => {
            log::warn!(
                "[insertion] clipboard re-open failed during restore: {}",
                err
            );
            clear_pending_clipboard_restore(restore_id);
            return;
        }
    };
    let current_text = match clipboard.get_text() {
        Ok(current) => Some(current),
        Err(err) => {
            log::warn!(
                "[insertion] clipboard get_text failed during restore: {}",
                err
            );
            None
        }
    };
    if should_restore_clipboard(current_text.as_deref(), &plan.inserted_text) {
        if let Some(previous_text) = original_text {
            if let Err(err) = clipboard.set_text(previous_text) {
                log::warn!("[insertion] clipboard restore failed: {}", err);
            }
        }
    } else {
        log::info!(
            "[insertion] skip clipboard restore: latest clipboard no longer matches inserted text"
        );
    }
    clear_pending_clipboard_restore(restore_id);
}
fn is_latest_clipboard_restore(restore_id: u64) -> bool {
    matches!(
        PENDING_CLIPBOARD_RESTORE.lock().as_ref(), Some(batch) if batch.latest_restore_id
        == restore_id
    )
}
fn clear_pending_clipboard_restore(restore_id: u64) {
    let mut pending = PENDING_CLIPBOARD_RESTORE.lock();
    if matches!(pending.as_ref(), Some(batch) if batch.latest_restore_id == restore_id) {
        pending.take();
    }
}
fn should_restore_clipboard(current_text: Option<&str>, inserted_text: &str) -> bool {
    matches!(current_text, Some(current) if current == inserted_text)
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
fn insertion_success_status() -> InsertStatus {
    InsertStatus::Inserted
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
    #[test]
    fn restore_only_when_clipboard_still_holds_inserted_text() {
        assert!(should_restore_clipboard(
            Some("dictated text"),
            "dictated text"
        ));
        assert!(!should_restore_clipboard(
            Some("user changed clipboard"),
            "dictated text"
        ));
        assert!(!should_restore_clipboard(None, "dictated text"));
    }
    #[test]
    fn empty_insertions_never_touch_clipboard_or_paste_path() {
        let inserter = TextInserter::new();
        assert_eq!(
            inserter.insert("", true, PasteShortcut::CtrlV),
            InsertStatus::CopiedFallback
        );
        assert_eq!(inserter.copy_fallback(""), InsertStatus::CopiedFallback);
    }
    #[test]
    fn pending_clipboard_restore_keeps_first_original_until_latest_restore() {
        *PENDING_CLIPBOARD_RESTORE.lock() = None;
        let (first_id, first_original) =
            remember_pending_clipboard_restore(Some("user clipboard".to_string()));
        let (second_id, second_original) =
            remember_pending_clipboard_restore(Some("first dictated text".to_string()));
        assert_ne!(first_id, second_id);
        assert_eq!(first_original.as_deref(), Some("user clipboard"));
        assert_eq!(second_original.as_deref(), Some("user clipboard"));
        assert!(!is_latest_clipboard_restore(first_id));
        assert!(is_latest_clipboard_restore(second_id));
        clear_pending_clipboard_restore(first_id);
        assert!(is_latest_clipboard_restore(second_id));
        clear_pending_clipboard_restore(second_id);
        assert!(!is_latest_clipboard_restore(second_id));
    }
    #[test]
    fn clipboard_restore_skips_when_clipboard_no_longer_matches_inserted_text() {
        assert!(should_restore_clipboard(
            Some("dictated text"),
            "dictated text"
        ));
        assert!(!should_restore_clipboard(
            Some("user edited clipboard"),
            "dictated text"
        ));
        assert!(!should_restore_clipboard(None, "dictated text"));
    }
    #[test]
    fn macos_paste_success_reports_inserted_and_guards_restore() {
        assert_eq!(insertion_success_status(), InsertStatus::Inserted);
        assert!(should_restore_clipboard(
            Some("dictated text"),
            "dictated text"
        ));
        assert!(!should_restore_clipboard(
            Some("user changed clipboard"),
            "dictated text"
        ));
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
