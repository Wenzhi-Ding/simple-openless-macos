//! macOS Accessibility 读取实现。
//!
//! 手写 FFI，与 `lib.rs::macos_capsule_ax` 的既有 ABI 写法同源（仓库没有
//! 引入 accessibility crate 的先例，这里保持一致）。新增的只有：`AXValue` 全文、
//! `kAXValueCFRangeType` 的 CFRange 解包、大文档走 `AXStringForRange` +
//! `AXNumberOfCharacters`，以及那两份旧代码都缺的 **messaging timeout**。
//!
//! ## 坐标系
//!
//! AX 的所有文本下标都是 **UTF-16 code unit**，而窗口算法按 char 走。中文在 UTF-16
//! 里 1 个单元、emoji 2 个，两套坐标必须显式换算 —— 见
//! [`utf16_offset_to_char_offset`](super::utf16_offset_to_char_offset)。
//!
//! ## 本文件只在 `spawn_blocking` 里跑
//!
//! 每个 AX 调用都可能阻塞到 `AX_MESSAGING_TIMEOUT_SECS`，绝不能出现在 tokio worker 上。
//! 调度由 [`super::probe_around_cursor`] 负责。

use super::{
    evaluate_gate, plan_window, utf16_offset_to_char_offset, window_around_cursor, GateInputs,
    ReadOutcome, AX_MESSAGING_TIMEOUT_SECS,
};
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;

/// 超过这个 UTF-16 长度就不整篇 `AXValue` 读回来，改走 `AXStringForRange` 只取光标附近。
///
/// 在一篇十万字的文档上 `AXValue` 会把整篇跨进程拷过来，光是 marshalling 就够撞上
/// 超时；而我们最终只要几百字。阈值取得比任何合理预算都大得多，正常文档仍走简单路径。
const FULL_TEXT_MAX_UTF16: usize = 20_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentLength {
    Unknown,
    WithinLimit(usize),
    OverLimit(usize),
}

fn classify_document_length(total: Option<usize>, limit: usize) -> DocumentLength {
    match total {
        None => DocumentLength::Unknown,
        Some(total) if total <= limit => DocumentLength::WithinLimit(total),
        Some(total) => DocumentLength::OverLimit(total),
    }
}

#[repr(C)]
struct OpaqueAxRef(c_void);
type AxUiElementRef = *mut OpaqueAxRef;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFTypeId = usize;
type AxError = i32;
type AxValueRef = *const c_void;

/// CoreFoundation 的 `CFRange`（`CFIndex` = `isize`）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CFRange {
    location: isize,
    length: isize,
}

const AX_ERROR_SUCCESS: AxError = 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_AX_VALUE_CF_RANGE_TYPE: i32 = 4;
/// `kCFNumberCFIndexType` —— 按 `CFIndex`（isize）取值，与 AX 的下标宽度一致。
const K_CF_NUMBER_CF_INDEX_TYPE: i32 = 14;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AxUiElementRef;
    fn AXUIElementGetPid(element: AxUiElementRef, pid: *mut i32) -> AxError;
    fn AXUIElementSetMessagingTimeout(element: AxUiElementRef, timeout: f32) -> AxError;
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
    fn AXValueCreate(value_type: i32, value_ptr: *const c_void) -> AxValueRef;
    /// 私有 SPI：把任意 AX 元素映射到「包含它的窗口」的 CGWindowID。
    /// 不在公开 SDK 头里，但自 10.9 起稳定，yabai / Rectangle / AeroSpace 都依赖它；
    /// 比读 _AXWindowID 字符串属性可靠（后者常不可用）。
    fn _AXUIElementGetWindow(element: AxUiElementRef, window_id: *mut u32) -> AxError;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeId;
    fn CFStringGetTypeID() -> CFTypeId;
    fn CFNumberGetTypeID() -> CFTypeId;
    fn CFStringCreateWithCString(
        allocator: CFAllocatorRef,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetCStringPtr(s: CFStringRef, encoding: u32) -> *const c_char;
    // CoreFoundation Boolean is unsigned char. This matches the generated
    // objc2-core-foundation binding (`type Boolean = u8`).
    fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringGetLength(s: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFNumberGetValue(number: CFTypeRef, number_type: i32, value_ptr: *mut c_void) -> u8;
}

/// 拿到焦点元素的结果。`Ready` 里的 ref **调用方负责 `CFRelease`**。
enum GatedElement {
    Ready(AxUiElementRef),
    Blocked(super::BlockReason),
    Unavailable(&'static str),
}

/// **拿到焦点元素的唯一入口 —— 想读宿主 app 的任何东西都必须从这里拿。**
///
/// 把「取元素」和「过闸门」焊死在一起，是因为它们分开过一次就出过事：闸门原本只装在
/// 读取路径上，手改观察器自己另开了一条取元素的路，于是在终端里听写时上下文读取被正确
/// 拦住、观察器却照样把终端全文读走。**闸门漏一条路径 = 没有闸门。**
///
/// 顺序有讲究，两段判定不能合并：
///
/// 1. 先用**前台 app**粗判一道（Secure Input、bundle 黑名单）—— 命中就一条 AX 消息都
///    不发，这是为了省事，不是最终判据；
/// 2. 拿到焦点元素后，用**元素自己的 pid** 换真正的 bundle，连同 `role` / `subrole`
///    再判一次 —— 这一道才算数。
///
/// 第二道为什么必须重新取 bundle：前台 app 是在取元素**之前**采样的，而每个 AX 调用
/// 都可能阻塞到 [`AX_MESSAGING_TIMEOUT_SECS`]。用户在这中间切了 app，第一道就会拿旧
/// app 的身份，放行一个属于新 app 的元素 —— 终端、密码管理器正是靠 bundle 黑名单拦的。
/// 拿元素自己的 pid 去问「你是谁」，这个时间窗就不存在了；顺带也修好了「焦点元素归属
/// 与前台 app 本来就可能不一致」这件事。
///
/// `AXUIElementSetMessagingTimeout` 也在这里统一设。不设就继承 AX 默认的 ~6 秒，对着
/// 一个卡死的 app 就是 6 秒冻结 —— 这是本模块最重要的一行。
unsafe fn focused_element_passing_the_gate(mut gate: GateInputs) -> GatedElement {
    if let Some(reason) = evaluate_gate(&gate) {
        return GatedElement::Blocked(reason);
    }

    let system = AXUIElementCreateSystemWide();
    if system.is_null() {
        return GatedElement::Unavailable("system-wide AX element unavailable");
    }
    // 系统级 element 上的设置会成为本进程的默认值。
    AXUIElementSetMessagingTimeout(system, AX_MESSAGING_TIMEOUT_SECS);

    let focused = copy_element_attr(system, b"AXFocusedUIElement\0");
    CFRelease(system as CFTypeRef);

    let Some(focused) = focused else {
        return GatedElement::Unavailable("no focused UI element (AX permission or no focus)");
    };
    // 显式再设一次：进程默认值只对「之后创建」的 ref 生效，对已有 ref 补一刀更稳。
    AXUIElementSetMessagingTimeout(focused, AX_MESSAGING_TIMEOUT_SECS);

    // 拿元素自己的身份重判，别再信第一道用的那个前台 app。
    //
    // **确认不了归属就不读 —— 这里必须失败关闭。** 取不到 pid 或查不到 bundle 时，
    // 如果沿用第一道那个采样值，闸门就退回按「谁在最前面」判定，等于这个修复没做；
    // 而把 `bundle_id` 清成 `None` 同样不行 —— `evaluate_gate` 对缺失的元数据是放行的
    //（见 `missing_metadata_does_not_block_by_itself`），那是另一种 fail-open。
    //
    // 代价是没有 bundle id 的进程读不到上下文。那类进程本来就很少，而「宁可不读」正是
    // 这个功能对隐私的基本承诺。
    let mut pid: i32 = 0;
    let owner = (AXUIElementGetPid(focused, &mut pid) == AX_ERROR_SUCCESS && pid > 0)
        .then(|| crate::focus_target::bundle_id_for_pid(pid))
        .flatten();
    let Some(owner) = owner else {
        CFRelease(focused as CFTypeRef);
        return GatedElement::Unavailable("could not confirm which app owns the focused element");
    };
    gate.bundle_id = Some(owner);
    // Secure Input 是全局状态，顺手也刷新一次 —— 同样可能在这几次 AX 调用期间才打开。
    gate.secure_input = crate::unicode_keystroke::is_secure_input_enabled();
    gate.role = copy_string_attr(focused, b"AXRole\0");
    gate.subrole = copy_string_attr(focused, b"AXSubrole\0");
    if let Some(reason) = evaluate_gate(&gate) {
        CFRelease(focused as CFTypeRef);
        return GatedElement::Blocked(reason);
    }

    GatedElement::Ready(focused)
}

/// 屏幕 OCR 复用的焦点安全闸门：与光标上下文同一条判定路径
/// （focused_element_passing_the_gate），但按 OCR 的合同收紧两点：
/// 1. 焦点元素的归属 pid 必须等于 expected_pid（触发那一刻冻结的前台 pid）；
/// 2. 焦点元素所在窗口的 CGWindowID（_AXUIElementGetWindow SPI）必须等于
///    expected_window_id——同 app 切到另一个窗口（例如弹出的密码面板）同样
///    拒绝，绝不用焦点闸门替别的窗口背书。
/// 拒绝按 [`super::OcrGateRejection`] 分类上抛：敏感状态与目标漂移由调用方
/// 硬拒绝；读不到焦点元素（Unavailable）是否降级放行由调用方按授权决定
/// （本函数自身不做这个决定）。只在专用 OCR 线程上调用（同步 AX，超时见
/// AX_MESSAGING_TIMEOUT_SECS）。
pub(super) fn ocr_focus_gate(
    expected_pid: i32,
    expected_window_id: u32,
) -> Result<(), super::OcrGateRejection> {
    use super::OcrGateRejection as Rejection;
    let (_, bundle_id) = crate::focus_target::current_front_app_parts();
    let gate = GateInputs {
        secure_input: crate::unicode_keystroke::is_secure_input_enabled(),
        bundle_id,
        role: None,
        subrole: None,
    };
    unsafe {
        match focused_element_passing_the_gate(gate) {
            GatedElement::Ready(element) => {
                let mut pid: i32 = 0;
                let pid_ok = AXUIElementGetPid(element, &mut pid) == AX_ERROR_SUCCESS;
                let window_matches = focused_window_matches(element, expected_window_id);
                CFRelease(element as CFTypeRef);
                if !pid_ok || pid != expected_pid {
                    Err(Rejection::Mismatch(
                        "focused element no longer belongs to the frozen app",
                    ))
                } else if !window_matches {
                    Err(Rejection::Mismatch(
                        "focused window is not the frozen window",
                    ))
                } else {
                    Ok(())
                }
            }
            GatedElement::Blocked(reason) => Err(Rejection::Sensitive(reason.as_str())),
            GatedElement::Unavailable(why) => Err(Rejection::Unavailable(why)),
        }
    }
}

/// 冻结目标用：焦点元素可读、归属 `expected_pid` 且能取到所在窗口时，返回该
/// 窗口的 CGWindowID；其余情况一律 None（调用方退回按窗口前后顺序选择）。
///
/// 与 [`ocr_focus_gate`] 刻意分开：这里不做黑名单/密码判定（freeze_target 在
/// 调用前已查过前台 bundle 与 Secure Input），只回答「用户此刻在哪个窗口里
/// 打字」，用来把冻结目标从「层序最前窗口」修正为「焦点所在窗口」——浏览器
/// 地址栏联想下拉这类弹窗会压在主窗口上面，纯按前后顺序会冻错对象。
/// 同步 AX（超时见 AX_MESSAGING_TIMEOUT_SECS），只在听写触发路径上调用一次。
pub(super) fn focused_window_id_for_pid(expected_pid: i32) -> Option<u32> {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(system, AX_MESSAGING_TIMEOUT_SECS);
        let focused = copy_element_attr(system, b"AXFocusedUIElement\0");
        CFRelease(system as CFTypeRef);
        let focused = focused?;
        AXUIElementSetMessagingTimeout(focused, AX_MESSAGING_TIMEOUT_SECS);
        let mut pid: i32 = 0;
        let mut window_id: u32 = 0;
        let ok = AXUIElementGetPid(focused, &mut pid) == AX_ERROR_SUCCESS
            && pid == expected_pid
            && _AXUIElementGetWindow(focused, &mut window_id) == AX_ERROR_SUCCESS
            && window_id != 0;
        CFRelease(focused as CFTypeRef);
        ok.then_some(window_id)
    }
}

/// 焦点元素所在窗口的 CGWindowID（_AXUIElementGetWindow SPI）是否等于冻结值。
/// SPI 返回非 0（失败）按 fail closed 处理。
unsafe fn focused_window_matches(focused: AxUiElementRef, expected_window_id: u32) -> bool {
    let mut window_id: u32 = 0;
    _AXUIElementGetWindow(focused, &mut window_id) == AX_ERROR_SUCCESS
        && window_id == expected_window_id
}

/// 负对照（测试用）：对一个没有窗口的系统级元素调用 SPI，必须返回失败
/// （非 0）而不是崩溃或成功——证明符号真实链接且失败路径按预期工作。
#[cfg(test)]
pub(super) fn ax_window_id_spi_fails_for_windowless_element() -> bool {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return false;
        }
        let mut window_id: u32 = 0;
        let err = _AXUIElementGetWindow(system, &mut window_id);
        CFRelease(system as CFTypeRef);
        err != AX_ERROR_SUCCESS
    }
}

/// 只读探针（测试用）：当前焦点元素的 (pid, 所在窗口 CGWindowID)。
/// 不读任何文本，只取身份；AX 不可用时返回 None。
#[cfg(test)]
pub(super) fn focused_identity_for_tests() -> Option<(i32, u32)> {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(system, AX_MESSAGING_TIMEOUT_SECS);
        let focused = copy_element_attr(system, b"AXFocusedUIElement\0");
        CFRelease(system as CFTypeRef);
        let focused = focused?;
        let mut pid: i32 = 0;
        let pid_ok = AXUIElementGetPid(focused, &mut pid) == AX_ERROR_SUCCESS && pid > 0;
        let mut window_id: u32 = 0;
        let window_ok = _AXUIElementGetWindow(focused, &mut window_id) == AX_ERROR_SUCCESS;
        CFRelease(focused as CFTypeRef);
        (pid_ok && window_ok).then_some((pid, window_id))
    }
}

/// 同步读取光标周围的文档。**只允许在 `spawn_blocking` 上下文里调用。**
///

/// `gate` 带着调用方已经填好的 `secure_input` / `bundle_id`；
/// [`focused_element_passing_the_gate`] 会补上 `role` / `subrole` 并做最终判定。
pub(super) fn read_around_cursor_blocking(budget_chars: usize, gate: GateInputs) -> ReadOutcome {
    unsafe {
        let focused = match focused_element_passing_the_gate(gate) {
            GatedElement::Ready(el) => el,
            GatedElement::Blocked(reason) => return ReadOutcome::Blocked(reason),
            GatedElement::Unavailable(why) => return ReadOutcome::Unavailable(why),
        };
        let outcome = read_document(focused, budget_chars);
        CFRelease(focused as CFTypeRef);
        outcome
    }
}

unsafe fn read_document(focused: AxUiElementRef, budget_chars: usize) -> ReadOutcome {
    let Some(cursor_utf16) = copy_caret_offset(focused) else {
        return ReadOutcome::Unavailable("AXSelectedTextRange unavailable (not a text element?)");
    };
    let total_utf16 = match classify_document_length(
        copy_index_attr(focused, b"AXNumberOfCharacters\0"),
        FULL_TEXT_MAX_UTF16,
    ) {
        DocumentLength::Unknown => {
            return ReadOutcome::Unavailable(
                "AXNumberOfCharacters unavailable; refusing an unbounded AXValue read",
            );
        }
        DocumentLength::WithinLimit(total) => {
            // 小文档（绝大多数情况）：整篇读回来，按 char 精确截窗。AXValue 不可读时
            // 仍可用已知总长度走下面的有界 AXStringForRange 回落。
            if let Some(text) = copy_string_attr(focused, b"AXValue\0") {
                let cursor = utf16_offset_to_char_offset(&text, cursor_utf16);
                return ReadOutcome::Window(window_around_cursor(&text, cursor, budget_chars));
            }
            total
        }
        DocumentLength::OverLimit(total) => total,
    };

    // 回落：文档太大，或者该控件压根不给 AXValue（Electron 类常见）。改成只跟它要
    // 光标附近的一段。UTF-16 预算给两倍 —— 宁可多要一点回来自己裁，也不要因为
    // char/UTF-16 换算差把上文截秃。
    let span = plan_window(total_utf16, cursor_utf16, budget_chars.saturating_mul(2));
    if span.len == 0 {
        return ReadOutcome::Window(super::DocumentWindow {
            text: String::new(),
            cursor: 0,
        });
    }
    let Some(text) = copy_string_for_range(focused, span.start, span.len) else {
        return ReadOutcome::Unavailable("AXStringForRange unavailable");
    };
    let cursor = utf16_offset_to_char_offset(&text, span.cursor_in_span);
    ReadOutcome::Window(window_around_cursor(&text, cursor, budget_chars))
}

/// 读 `AXSelectedTextRange` 的起点 —— 没有选区时它就是光标位置（length == 0）。
unsafe fn copy_caret_offset(focused: AxUiElementRef) -> Option<usize> {
    let range = copy_selected_range(focused)?;
    caret_offset_from_location(range.location)
}

/// 把 `AXSelectedTextRange` 的 location 翻成光标偏移。**负数是「没有光标」，不是 0。**
///
/// 部分 app（尤其 Electron 那一类）在没有插入点或元素不是文本控件时返回
/// `kCFNotFound`（-1）。原本这里 `.max(0)`，等于把「不知道光标在哪」当成「光标在开头」
/// —— 于是我们读回文档**开头**那几百个字，再当作「光标附近」发给 LLM。错得静默：
/// 日志里看到的是 `before=0 after=N`，像是「上文为空」，实际是读错了地方。
///
/// 返回 `None` 让 `read_document` 走 `Unavailable` 分支：这次不发上下文，探针里也能
/// 看到原因。宁可没有上下文，不要错的上下文。
fn caret_offset_from_location(location: isize) -> Option<usize> {
    (location >= 0).then_some(location as usize)
}

unsafe fn copy_selected_range(focused: AxUiElementRef) -> Option<CFRange> {
    let value = copy_attr(focused, b"AXSelectedTextRange\0")?;
    let mut range = CFRange::default();
    let ok = AXValueGetValue(
        value as AxValueRef,
        K_AX_VALUE_CF_RANGE_TYPE,
        &mut range as *mut _ as *mut c_void,
    );
    CFRelease(value);
    (ok != 0).then_some(range)
}

/// `AXStringForRange(range)` —— 只把光标附近那段跨进程拷回来。
unsafe fn copy_string_for_range(
    focused: AxUiElementRef,
    start: usize,
    len: usize,
) -> Option<String> {
    let attr = cfstring_from_static(b"AXStringForRange\0")?;
    let range = CFRange {
        location: start as isize,
        length: len as isize,
    };
    let range_value = AXValueCreate(
        K_AX_VALUE_CF_RANGE_TYPE,
        &range as *const _ as *const c_void,
    );
    if range_value.is_null() {
        CFRelease(attr);
        return None;
    }

    let mut out: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyParameterizedAttributeValue(focused, attr, range_value, &mut out);
    CFRelease(attr);
    CFRelease(range_value);
    if err != AX_ERROR_SUCCESS || out.is_null() {
        return None;
    }

    let text = if CFGetTypeID(out) == CFStringGetTypeID() {
        cfstring_to_rust(out)
    } else {
        None
    };
    CFRelease(out);
    text
}

/// 读一个属性并保证它真的是 CFString。
///
/// 类型检查不是多余的：`AXValue` 在滑块上是数字、在复选框上是布尔。不检查就会把
/// 一个 CFNumber 当字符串解，轻则乱码重则读越界。
unsafe fn copy_string_attr(element: AxUiElementRef, attribute: &[u8]) -> Option<String> {
    let value = copy_attr(element, attribute)?;
    let text = if CFGetTypeID(value) == CFStringGetTypeID() {
        cfstring_to_rust(value)
    } else {
        None
    };
    CFRelease(value);
    text
}

/// 读一个 CFNumber 属性并按 `CFIndex` 取值。
unsafe fn copy_index_attr(element: AxUiElementRef, attribute: &[u8]) -> Option<usize> {
    let value = copy_attr(element, attribute)?;
    if CFGetTypeID(value) != CFNumberGetTypeID() {
        CFRelease(value);
        return None;
    }
    let mut out: isize = 0;
    let ok = CFNumberGetValue(
        value,
        K_CF_NUMBER_CF_INDEX_TYPE,
        &mut out as *mut _ as *mut c_void,
    );
    CFRelease(value);
    if ok != 0 && out >= 0 {
        Some(out as usize)
    } else {
        None
    }
}

/// 读一个属性，值本身就是另一个 AXUIElement（如 `AXFocusedUIElement`）。
unsafe fn copy_element_attr(element: AxUiElementRef, attribute: &[u8]) -> Option<AxUiElementRef> {
    copy_attr(element, attribute).map(|value| value as AxUiElementRef)
}

/// 读任意属性的原始 CFTypeRef。**调用方负责 `CFRelease`。**
unsafe fn copy_attr(element: AxUiElementRef, attribute: &[u8]) -> Option<CFTypeRef> {
    let attr = cfstring_from_static(attribute)?;
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr, &mut value);
    CFRelease(attr);
    if err != AX_ERROR_SUCCESS || value.is_null() {
        None
    } else {
        Some(value)
    }
}

unsafe fn cfstring_from_static(bytes_with_nul: &[u8]) -> Option<CFStringRef> {
    let cstr = CStr::from_bytes_with_nul(bytes_with_nul).ok()?;
    let s = CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8);
    if s.is_null() {
        None
    } else {
        Some(s)
    }
}

unsafe fn cfstring_to_rust(s: CFStringRef) -> Option<String> {
    let direct = CFStringGetCStringPtr(s, K_CF_STRING_ENCODING_UTF8);
    if !direct.is_null() {
        return CStr::from_ptr(direct).to_str().ok().map(str::to_string);
    }
    let length = CFStringGetLength(s);
    if length <= 0 {
        return Some(String::new());
    }
    let max_bytes = CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8) + 1;
    let mut buf: Vec<u8> = vec![0; max_bytes as usize];
    let ok = CFStringGetCString(
        s,
        buf.as_mut_ptr() as *mut c_char,
        max_bytes,
        K_CF_STRING_ENCODING_UTF8,
    );
    if ok == 0 {
        return None;
    }
    CStr::from_ptr(buf.as_ptr() as *const c_char)
        .to_str()
        .ok()
        .map(str::to_string)
}
