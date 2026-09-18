//! macOS 屏幕 OCR 后台观测（RapidOCR worker，纯内存）。
//!
//! 听写触发那一刻 `start` 在调用线程上冻结前台目标（pid + window id），随后把
//! 截图搬到本模块自己的线程；识别交给常驻的 `ocr_engine`（RapidOCR Python
//! worker，离线自防护见 ocr_worker.py），录音 start 绝不为 OCR 等待。结果经
//! `openless_core::ports::ScreenOcrSlot` 在润色输入冻结点被非阻塞收取。
//!
//! ## 安全模型（全部绑定「冻结那一刻」的目标，后台只做复核、不做替换）
//!
//! 冻结时：前台 pid/bundle + Secure Input + bundle 黑名单（复用
//! `crate::host_document` 的闸门常量）；冻结窗口优先取焦点所在窗口（焦点
//! 可读时——浏览器联想下拉这类弹窗会压在主窗口上面，纯按前后顺序会冻错
//! 对象），焦点不可读退回该 pid 最前面的普通层窗口。我们自己的 pid
//! （胶囊/弹出层）永不截取。
//!
//! 后台复核（截图只截冻结窗口，绝不退整屏）：
//! 1. 冻结窗口仍有效：焦点冻结模式要求它仍在屏且仍属冻结 pid（弹窗可能
//!    压在它上面，层序最前不等于它）；层序冻结模式要求它仍是该 pid 的
//!    **最前**普通层窗口——同 app 切到别的窗口即拒绝；
//! 2. 当前前台 pid 仍是冻结 pid（用户切走 app = 放弃本次观测）；
//! 3. `crate::host_document::screen_ocr_focus_gate`：敏感状态（Secure
//!    Input / 敏感应用 / 密码框）与焦点漂移（归属 pid 或所在窗口与冻结
//!    目标不符）一律硬拒绝；**焦点不可读（目标 app 不汇报焦点元素）时按
//!    2026-09-18 用户授权降级放行**——此时黑名单与 Secure Input 已在闸门
//!    第一阶段查过，叠加第 1、2 两条复核后截图，诊断如实标注焦点校验未
//!    执行。降级路径丢掉的保护只有一项：网页密码框的 AX 角色检查（原生
//!    密码框仍由 Secure Input 系统级拦截）；
//! 4. CGPreflightScreenCaptureAccess 仅为预检、**从不弹窗**（授权只能由用户在
//!    设置页主动触发，见 permissions 命令层）。
//!
//! ## 取消与超时
//!
//! observe 在截图前与 OCR 前各查一次槽位状态与截止时间：cancel/新会话/开关
//! 关闭（槽位 Discarded）或润色输入已冻结探测过（槽位 Done）后，后台线程
//! 不会再截屏；超时（SCREEN_OCR_DEADLINE）同样作废。并发观测线程数有上限，
//! 超限时直接放弃本次观测。
//!
//! ## 内存与日志
//!
//! 所有后台 ObjC 执行都包在 autoreleasepool 里；alloc/init 所得 +1 引用与
//! RAII 包装（CFArray/CFDictionary/CFString wrapper 的 Drop）一一配对，
//! getter / 错误出参等非持有引用一律不做手动 release。截图只存在于内存；日志
//! 只记状态词与字符数，不记 OCR 正文。每会话至多一次，无跨会话缓存。
//! 进 prompt 的处理后 OCR 正文随润色诊断快照（polishDiagnostics）保存在本机
//! history.json（2026-09-08 用户明确授权，仅本机、不上传、不进日志）；截图与
//! 偏好仍然不存任何内容。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use openless_core::ports::{ScreenOcrOutcome, ScreenOcrSlot, ScreenOcrStart};

/// 整次观测（复核 + 截图 + OCR）的后台硬上限。Vision 是同步 API、没有内建
/// 超时；超时后槽位作废，被拖住的工作线程在下一个检查点自行退出（其结果
/// 投递到已作废槽位即丢弃）。
pub(crate) const SCREEN_OCR_DEADLINE: Duration = Duration::from_secs(5);

/// 同时在跑的观测线程上限。正常情况下同一时刻至多一个会话在听写；上限只防
/// 病理性堆积（连续快速会话 + 卡死的 Vision 调用）。达到上限时放弃新观测。
const MAX_INFLIGHT_OBSERVATIONS: usize = 2;

fn inflight_observations() -> &'static AtomicUsize {
    static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
    &INFLIGHT
}

/// 观测线程存续期间的在跑计数守卫（Drop 递减）。
struct InflightGuard;

impl InflightGuard {
    fn try_acquire() -> Option<Self> {
        let current = inflight_observations().fetch_add(1, Ordering::AcqRel);
        if current >= MAX_INFLIGHT_OBSERVATIONS {
            inflight_observations().fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self)
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        inflight_observations().fetch_sub(1, Ordering::AcqRel);
    }
}

// ─────────────────────────── 槽位 ───────────────────────────

#[derive(Debug)]
enum SlotState {
    Pending,
    Ready(String),
    /// 结果已被消费。
    Taken,
    /// 终态：探测过但当时未就绪（润色输入已冻结，None），或 worker 记录了
    /// 轻量拒绝原因（Some）。此后 complete 无效。
    Done(Option<ScreenOcrOutcome>),
    /// 被作废（cancel / 新会话 / 开关关闭）——终态。
    Discarded,
}

/// Core 侧 `ScreenOcrSlot` 的实现。状态机：Pending --(worker 投递)--> Ready
/// --(try_take)--> Taken；Pending --(try_take 未就绪)--> Done；任何状态
/// --(discard)--> Discarded。终态后晚到的识别结果直接丢弃。
pub(crate) struct ScreenOcrSlotImpl {
    state: Mutex<SlotState>,
    /// 本次观测是否在焦点不可达时走了降级放行路径（2026-09-18 用户授权）。
    /// 只供诊断如实标注，与正文路径无关。
    focus_gate_relaxed: AtomicBool,
}

impl Default for ScreenOcrSlotImpl {
    fn default() -> Self {
        Self {
            state: Mutex::new(SlotState::Pending),
            focus_gate_relaxed: AtomicBool::new(false),
        }
    }
}

impl ScreenOcrSlotImpl {
    /// 识别完成：仅当仍处于 Pending 才可见。不记任何内容进日志。
    fn complete(&self, text: String) {
        let mut guard = self.state.lock().expect("screen-ocr slot lock poisoned");
        if matches!(*guard, SlotState::Pending) {
            *guard = SlotState::Ready(text);
        }
    }

    /// 观测线程记录轻量终态原因（超时/目标漂移/权限/敏感拦截/失败）。仅 Pending
    /// 可写入；不记任何正文。
    fn finish(&self, outcome: ScreenOcrOutcome) {
        let mut guard = self.state.lock().expect("screen-ocr slot lock poisoned");
        if matches!(*guard, SlotState::Pending) {
            *guard = SlotState::Done(Some(outcome));
        }
    }

    /// 槽位是否仍可能产生结果（观测线程据此在截图/OCR 前中止）。
    fn alive(&self) -> bool {
        matches!(
            *self.state.lock().expect("screen-ocr slot lock poisoned"),
            SlotState::Pending | SlotState::Ready(_)
        )
    }

    /// 观测线程在降级放行时调用；必须先于 `complete`（先立标记再交正文，
    /// core 在冻结点收正文时读到的标记才属于这次观测）。
    fn mark_focus_gate_relaxed(&self) {
        self.focus_gate_relaxed.store(true, Ordering::SeqCst);
    }
}

impl ScreenOcrSlot for ScreenOcrSlotImpl {
    fn try_take(&self) -> Option<String> {
        let mut guard = self.state.lock().expect("screen-ocr slot lock poisoned");
        match std::mem::replace(&mut *guard, SlotState::Taken) {
            SlotState::Ready(text) => Some(text),
            // 未就绪的探测也是终态：润色输入已冻结，此后没人再来问，
            // 晚到的 complete 不得把 Done 翻回 Ready。
            SlotState::Pending => {
                *guard = SlotState::Done(None);
                None
            }
            // 已终结的槽位保持原终态（含 worker 记录的轻量原因）：冻结点先
            // try_take 再读 outcome，wipe 成 Taken 会把「为什么没有这段上下文」
            // 一并抹掉。
            terminal => {
                *guard = terminal;
                None
            }
        }
    }

    fn discard(&self) {
        *self.state.lock().expect("screen-ocr slot lock poisoned") = SlotState::Discarded;
    }

    fn outcome(&self) -> Option<ScreenOcrOutcome> {
        match *self.state.lock().expect("screen-ocr slot lock poisoned") {
            SlotState::Done(Some(outcome)) => Some(outcome),
            SlotState::Discarded => Some(ScreenOcrOutcome::Discarded),
            // Pending / Ready / Taken / Done(None)：要么还没终态，要么正文路径
            // 已经由 try_take 表达，core 不需要 outcome。
            _ => None,
        }
    }

    fn focus_gate_relaxed(&self) -> bool {
        self.focus_gate_relaxed.load(Ordering::SeqCst)
    }
}

// ─────────────────────── 后台监督（超时） ───────────────────────

/// 在独立线程上跑 `work`，最多等 `deadline`。超时/线程失败返回对应轻量结局
/// （调用方据此作废槽位；工作线程在自身检查点自行退出）。抽出来是为了能用
/// 假工作负载单测超时语义。
fn run_with_deadline<F>(deadline: Duration, work: F) -> Result<String, ScreenOcrOutcome>
where
    F: FnOnce() -> Result<String, ScreenOcrOutcome> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, ScreenOcrOutcome>>();
    let spawned = std::thread::Builder::new()
        .name("openless-screen-ocr-work".into())
        .spawn(move || {
            let result = work();
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        return Err(ScreenOcrOutcome::Failed);
    }
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(_) => Err(ScreenOcrOutcome::Timeout),
    }
}

// ──────────────────── 窗口枚举（纯函数可测） ────────────────────

/// CGWindowListCopyWindowInfo 返回条目的最小投影。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowInfo {
    pub id: u32,
    pub owner_pid: i32,
    pub layer: i32,
}

/// 从前到后的窗口列表里选出「属于 front_pid 的最前面普通层窗口」。
/// 列表顺序由 CGWindowListCopyWindowInfo 保证（前到后）；layer 0 是普通窗口，
/// 菜单栏/桌面元素/浮层全部排除。找不到 = 无法确认目标 → None（fail closed）。
/// 复核时要求本函数结果**恰好等于**冻结的 window id——同 app 切换窗口即拒绝。
pub(crate) fn choose_frontmost_window(windows: &[WindowInfo], front_pid: i32) -> Option<u32> {
    windows
        .iter()
        .find(|window| window.owner_pid == front_pid && window.layer == 0)
        .map(|window| window.id)
}

/// 冻结选窗（2026-09-18 授权修正）：焦点所在窗口（AX 读出、且确实在窗口
/// 列表里、确实属该 pid）优先——浏览器联想下拉这类弹窗会压在主窗口上面，
/// 纯按前后顺序会把弹窗冻成目标，复核时焦点窗口对不上只能整次作废；
/// 读不到焦点或焦点窗口对不上列表时退回前后顺序选择。返回 (window_id,
/// 是否按焦点冻结)，复核逻辑按这个标记区分口径。
pub(crate) fn pick_frozen_window(
    windows: &[WindowInfo],
    front_pid: i32,
    focused_window_id: Option<u32>,
) -> Option<(u32, bool)> {
    let top = choose_frontmost_window(windows, front_pid)?;
    match focused_window_id {
        Some(id)
            if windows
                .iter()
                .any(|window| window.id == id && window.owner_pid == front_pid) =>
        {
            Some((id, true))
        }
        _ => Some((top, false)),
    }
}

/// 复核冻结窗口是否仍有效。焦点冻结模式：弹窗可能仍压在它上面，层序最前
/// 不等于它，只要求窗口仍在屏且仍属冻结 pid（截图按窗口号只截它自己）；
/// 层序冻结模式：它必须仍是该 pid 最前的普通层窗口，同 app 切窗口即失效。
pub(crate) fn frozen_window_still_valid(
    windows: &[WindowInfo],
    frozen_pid: i32,
    window_id: u32,
    frozen_via_focus: bool,
) -> bool {
    if frozen_via_focus {
        windows
            .iter()
            .any(|window| window.id == window_id && window.owner_pid == frozen_pid)
    } else {
        choose_frontmost_window(windows, frozen_pid) == Some(window_id)
    }
}

// ──────────────────── 屏幕录制权限（预检，不弹窗） ────────────────────

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    /// 会触发系统授权流程——**只允许**用户在设置页主动点击时调用
    /// （见 permissions 命令层），自动路径一律用 preflight。
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// 是否已拥有屏幕录制权限。仅预检，绝不弹窗。
#[cfg(target_os = "macos")]
pub(crate) fn screen_capture_allowed() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// 用户在设置页主动请求屏幕录制权限（会弹系统授权）。返回请求后是否已授权。
#[cfg(target_os = "macos")]
pub fn request_screen_capture_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

// ─────────────────────────── macOS 实现 ───────────────────────────

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        frozen_window_still_valid, pick_frozen_window, run_with_deadline, InflightGuard,
        ScreenOcrSlotImpl, WindowInfo, SCREEN_OCR_DEADLINE,
    };
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use core_graphics::image::CGImage;
    use core_graphics::window as cg_window;
    use foreign_types::ForeignType as _;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use openless_core::ports::{
        ScreenOcrNotStarted, ScreenOcrOutcome, ScreenOcrSlot, ScreenOcrStart,
    };
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::time::Instant;

    /// CGWindowListCreateImage 拒绝截取的超大窗口（像素数）。正常窗口远低于此；
    /// 超大 = 目标状态异常，宁可跳过也别让 OCR 吃满 5 秒预算。
    const MAX_WINDOW_PIXELS: usize = 40_000_000;

    // CGImage → PNG（仅内存）。ImageIO 的 C API：type identifier 用 UTType
    // 旧式字符串 "public.png"；CFMutableData 起手 0 容量自动增长。
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreateMutable(allocator: *const c_void, capacity: isize) -> *mut c_void;
        fn CFDataGetLength(data: *const c_void) -> isize;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            cstr: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const c_void;
    }
    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageDestinationCreateWithData(
            data: *const c_void,
            type_identifier: *const c_void,
            count: usize,
            options: *const c_void,
        ) -> *mut c_void;
        fn CGImageDestinationAddImage(
            dest: *const c_void,
            image: *const c_void,
            properties: *const c_void,
        );
        fn CGImageDestinationFinalize(dest: *const c_void) -> bool;
    }

    /// 编码 CGImage 为 PNG 字节（纯内存；失败返回 None，绝不落盘）。
    fn encode_png(image: &CGImage) -> Option<Vec<u8>> {
        const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
        unsafe {
            let data = CFDataCreateMutable(std::ptr::null(), 0);
            if data.is_null() {
                return None;
            }
            let result = (|| {
                let type_id = CFStringCreateWithCString(
                    std::ptr::null(),
                    b"public.png\0".as_ptr() as *const std::ffi::c_char,
                    K_CF_STRING_ENCODING_UTF8,
                );
                if type_id.is_null() {
                    return None;
                }
                let dest = CGImageDestinationCreateWithData(
                    data,
                    type_id,
                    1,
                    std::ptr::null(),
                );
                core_foundation::base::CFRelease(type_id);
                if dest.is_null() {
                    return None;
                }
                CGImageDestinationAddImage(dest, image.as_ptr() as *const c_void, std::ptr::null());
                let ok = CGImageDestinationFinalize(dest);
                core_foundation::base::CFRelease(dest as *const c_void);
                if !ok {
                    return None;
                }
                let length = CFDataGetLength(data);
                let pointer = CFDataGetBytePtr(data);
                if length <= 0 || pointer.is_null() {
                    return None;
                }
                Some(std::slice::from_raw_parts(pointer, length as usize).to_vec())
            })();
            core_foundation::base::CFRelease(data as *const c_void);
            result
        }
    }

    /// NSWorkspace frontmostApplication 的 (pid, bundle id)。与 focus_target.rs 同一
    /// msg_send 模式（仓库无 NSWorkspace 绑定先例）。processIdentifier 返回
    /// pid_t（i32，编码 "i"），不能按 isize 收。bundleIdentifier 是
    /// ARC 框架属性 getter（返回 +1），用 Retained::from_raw 接管所有权。
    fn frontmost_pid_and_bundle() -> Option<(i32, String)> {
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return None;
            }
            let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return None;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid <= 0 {
                return None;
            }
            // bundleIdentifier 按借用处理：CFString get-rule 包装自带 +1/-1 配对；
            // 若框架按 ARC 返回 +1，则这里至多漏掉这一次引用（每会话一次，可忽略），
            // 绝不 over-release（from_raw 接管在 autoreleased 约定下会在池回收后崩溃）。
            let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
            if bundle.is_null() {
                return None;
            }
            let text = CFString::wrap_under_get_rule(bundle as *const _).to_string();
            Some((pid, text))
        }
    }

    /// 枚举在屏窗口（前到后），取 (window id, owner pid, layer) 投影。
    fn list_onscreen_windows() -> Option<Vec<WindowInfo>> {
        let array = cg_window::copy_window_info(
            cg_window::kCGWindowListOptionOnScreenOnly,
            cg_window::kCGNullWindowID,
        )?;
        let key_number = unsafe { CFString::wrap_under_get_rule(cg_window::kCGWindowNumber) };
        let key_pid = unsafe { CFString::wrap_under_get_rule(cg_window::kCGWindowOwnerPID) };
        let key_layer = unsafe { CFString::wrap_under_get_rule(cg_window::kCGWindowLayer) };
        let mut windows = Vec::with_capacity(array.len() as usize);
        for index in 0..array.len() {
            let item = array.get(index)?;
            let dict =
                unsafe { CFDictionary::<CFString, CFType>::wrap_under_get_rule((*item) as *mut _) };
            let integer = |key: &CFString| -> Option<i64> {
                dict.find(key)
                    .and_then(|value| value.downcast::<CFNumber>())
                    .and_then(|number| number.to_i64())
            };
            let (Some(id), Some(pid), layer) = (
                integer(&key_number),
                integer(&key_pid),
                integer(&key_layer).unwrap_or(-1),
            ) else {
                continue;
            };
            if id <= 0 || pid <= 0 {
                continue;
            }
            windows.push(WindowInfo {
                id: id as u32,
                owner_pid: pid as i32,
                layer: layer as i32,
            });
        }
        Some(windows)
    }

    /// 仅截冻结的那一个窗口（CGRectNull = 整窗；失败返回 None，绝不退整屏）。
    fn capture_window_image(window_id: u32) -> Option<CGImage> {
        let rect_null = CGRect::new(
            &CGPoint::new(f64::INFINITY, f64::INFINITY),
            &CGSize::new(0.0, 0.0),
        );
        let image = cg_window::create_image(
            rect_null,
            cg_window::kCGWindowListOptionIncludingWindow,
            window_id,
            cg_window::kCGWindowImageBoundsIgnoreFraming
                | cg_window::kCGWindowImageNominalResolution,
        )?;
        let pixels = (image.width() as usize).saturating_mul(image.height() as usize);
        if pixels == 0 || pixels > MAX_WINDOW_PIXELS {
            log::info!("[screen-ocr] skip: implausible window image ({pixels} px)");
            return None;
        }
        Some(image)
    }

    /// 截图 → RapidOCR worker（2026-09-18 由 Apple Vision 切换；对比实验见
    /// .temp/ocr-bench-2026-09-18/）。PNG 只在内存中经 base64 过管道，不落盘；
    /// worker 侧离线自防护见 ocr_worker.py。deadline 用尽或 worker 故障即 None
    /// （调用方映射为 Failed，绝不让一次观测拖住录音）。
    fn recognize_text(image: &CGImage, deadline: Instant) -> Option<String> {
        let png = encode_png(image)?;
        crate::ocr_engine::ocr_png(&png, deadline)
    }

    /// 后台复核 + 截图 + OCR。复核全部绑定冻结的目标，并在**截图前与 OCR 前**
    /// 各检查一次槽位与截止时间——cancel/新会话/开关关闭/润色已冻结之后，
    /// 这里不会再产生任何截屏。任一复核不符直接放弃，绝不为「此刻的前台」
    /// 截一张替冻结窗口背书的图。
    fn observe(
        window_id: u32,
        frozen_pid: i32,
        frozen_via_focus: bool,
        deadline: Instant,
        slot: &ScreenOcrSlotImpl,
    ) -> Result<String, ScreenOcrOutcome> {
        let bail = |reason: &str, outcome: ScreenOcrOutcome| -> Result<String, ScreenOcrOutcome> {
            log::info!("[screen-ocr] skip: {reason}");
            Err(outcome)
        };
        if !slot.alive() {
            return bail(
                "slot already finished or discarded",
                ScreenOcrOutcome::Discarded,
            );
        }
        if Instant::now() >= deadline {
            return bail(
                "deadline reached before verification",
                ScreenOcrOutcome::Timeout,
            );
        }

        let windows = list_onscreen_windows().ok_or(ScreenOcrOutcome::Failed)?;
        // 同 app 切换窗口也拒绝：复核口径随冻结方式（焦点/层序）分派。
        if !frozen_window_still_valid(&windows, frozen_pid, window_id, frozen_via_focus) {
            return bail(
                "frontmost window of the frozen app changed",
                ScreenOcrOutcome::TargetChanged,
            );
        }
        let (current_pid, _) = frontmost_pid_and_bundle().ok_or(ScreenOcrOutcome::Failed)?;
        if current_pid != frozen_pid {
            return bail(
                "frontmost application changed before capture",
                ScreenOcrOutcome::TargetChanged,
            );
        }
        let mut focus_gate_relaxed = false;
        match crate::host_document::screen_ocr_focus_gate(frozen_pid, window_id) {
            Ok(()) => {}
            Err(crate::host_document::OcrGateRejection::Sensitive(reason)) => {
                return bail(reason, ScreenOcrOutcome::SensitiveBlocked);
            }
            Err(crate::host_document::OcrGateRejection::Mismatch(reason)) => {
                return bail(reason, ScreenOcrOutcome::TargetChanged);
            }
            // 目标 app 汇报不了焦点元素（网页套壳 / Electron 等）：按
            // 2026-09-18 用户授权降级放行。此时黑名单与 Secure Input 已在闸门
            // 第一阶段查过，前台 pid 与冻结窗口有效性也已复核；截图按窗口号
            // 只截冻结窗口本身。标记进槽位，诊断里如实展示。
            Err(crate::host_document::OcrGateRejection::Unavailable(reason)) => {
                log::info!(
                    "[screen-ocr] focus gate unavailable ({reason}); capturing with non-focus checks"
                );
                focus_gate_relaxed = true;
            }
        }
        if !super::screen_capture_allowed() {
            return bail(
                "screen recording permission not granted",
                ScreenOcrOutcome::PermissionDenied,
            );
        }

        if !slot.alive() {
            return bail(
                "slot finished or discarded before capture",
                ScreenOcrOutcome::Discarded,
            );
        }
        if Instant::now() >= deadline {
            return bail("deadline reached before capture", ScreenOcrOutcome::Timeout);
        }
        let image = capture_window_image(window_id).ok_or(ScreenOcrOutcome::Failed)?;

        if !slot.alive() {
            return bail(
                "slot finished or discarded before recognition",
                ScreenOcrOutcome::Discarded,
            );
        }
        if Instant::now() >= deadline {
            return bail(
                "deadline reached before recognition",
                ScreenOcrOutcome::Timeout,
            );
        }
        let text = recognize_text(&image, deadline).ok_or(ScreenOcrOutcome::Failed)?;
        if focus_gate_relaxed {
            slot.mark_focus_gate_relaxed();
        }
        Ok(text)
    }

    pub(super) fn start_macos() -> ScreenOcrStart {
        let started = Instant::now();
        let guard = InflightGuard::try_acquire();
        if guard.is_none() {
            log::info!("[screen-ocr] skip: too many observations in flight");
            return ScreenOcrStart::not_started(ScreenOcrNotStarted::Busy);
        }

        // 冻结与后台全程各包一层 autoreleasepool：ObjC 自动释放对象随池回收。
        let frozen = autoreleasepool(|_| freeze_target());
        let (pid, window_id, via_focus) = match frozen {
            Ok(frozen) => frozen,
            Err(reason) => return ScreenOcrStart::not_started(reason),
        };

        let slot = Arc::new(ScreenOcrSlotImpl::default());
        let worker_slot = Arc::clone(&slot);
        let deadline = Instant::now() + SCREEN_OCR_DEADLINE;
        let spawned = std::thread::Builder::new()
            .name("openless-screen-ocr".into())
            .spawn(move || {
                let _inflight = guard;
                let work_slot = Arc::clone(&worker_slot);
                let outcome = autoreleasepool(|_| {
                    run_with_deadline(SCREEN_OCR_DEADLINE, move || {
                        observe(window_id, pid, via_focus, deadline, &work_slot)
                    })
                });
                // elapsed 在工作完成后才计量（此前版本在工作前取值，时长是假的）。
                let elapsed = started.elapsed();
                match outcome {
                    Ok(text) if !text.trim().is_empty() => {
                        log::info!(
                            "[screen-ocr] ready in {elapsed:?} ({} chars)",
                            text.chars().count()
                        );
                        worker_slot.complete(text);
                    }
                    // 失败/空白/超时把轻量原因写进终态：不留一个永远 Pending 的对象，
                    // 诊断也能拿到「为什么没有这段上下文」。识别返回空文本按失败计
                    // （recognize_text 正常不会给空 Some，防御分支）。
                    Ok(_) => worker_slot.finish(ScreenOcrOutcome::Failed),
                    Err(outcome) => worker_slot.finish(outcome),
                }
            });
        if spawned.is_err() {
            log::warn!("[screen-ocr] failed to spawn the observation thread");
            return ScreenOcrStart::not_started(ScreenOcrNotStarted::SpawnFailed);
        }
        ScreenOcrStart::started(slot as Arc<dyn ScreenOcrSlot>)
    }

    #[cfg(test)]
    pub(super) fn list_onscreen_windows_for_tests() -> Option<Vec<WindowInfo>> {
        autoreleasepool(|_| list_onscreen_windows())
    }

    #[cfg(test)]
    pub(super) fn frontmost_pid_and_bundle_for_tests() -> Option<(i32, String)> {
        autoreleasepool(|_| frontmost_pid_and_bundle())
    }

    #[cfg(test)]
    pub(super) fn encode_png_for_tests(image: &CGImage) -> Option<Vec<u8>> {
        encode_png(image)
    }

    /// 调用线程上的目标冻结。返回 (pid, window_id, 是否按焦点冻结)；
    /// 拒绝原因以轻量枚举上抛（诊断要能说清「为什么没有这段上下文」）。
    /// 为把冻结目标从「层序最前窗口」修正为「焦点所在窗口」，这里做了一次
    /// 同步 AX 读（超时见 AX_MESSAGING_TIMEOUT_SECS，读不到立即退回层序
    /// 选择，不影响其余冻结检查）。
    fn freeze_target() -> Result<(i32, u32, bool), ScreenOcrNotStarted> {
        let Some((pid, bundle)) = frontmost_pid_and_bundle() else {
            log::info!("[screen-ocr] skip: no frontmost application");
            return Err(ScreenOcrNotStarted::NoFrontmostApp);
        };
        if pid == std::process::id() as i32 {
            log::info!("[screen-ocr] skip: frontmost is OpenLess itself");
            return Err(ScreenOcrNotStarted::SelfFrontmost);
        }
        if crate::unicode_keystroke::is_secure_input_enabled() {
            log::info!("[screen-ocr] skip: secure input active at trigger");
            return Err(ScreenOcrNotStarted::SecureInput);
        }
        if crate::host_document::evaluate_gate(&crate::host_document::GateInputs {
            secure_input: false,
            bundle_id: Some(bundle),
            role: None,
            subrole: None,
        })
        .is_some()
        {
            log::info!("[screen-ocr] skip: frontmost app is deny-listed at trigger");
            return Err(ScreenOcrNotStarted::SensitiveApp);
        }
        let Some(windows) = list_onscreen_windows() else {
            log::info!("[screen-ocr] skip: window list unavailable at trigger");
            return Err(ScreenOcrNotStarted::WindowUnavailable);
        };
        let focused_window_id = crate::host_document::focused_window_id_for_pid(pid);
        let Some((window_id, via_focus)) =
            pick_frozen_window(&windows, pid, focused_window_id)
        else {
            log::info!("[screen-ocr] skip: no normal-layer window for the frozen app");
            return Err(ScreenOcrNotStarted::WindowUnavailable);
        };
        Ok((pid, window_id, via_focus))
    }
}

/// 冻结前台目标并启动后台观测；仅当用户打开偏好开关时被 core 调用（见
/// api.rs capture_dictation_context）。非 macOS 无实现，返回 NotStarted。
pub fn start() -> ScreenOcrStart {
    #[cfg(target_os = "macos")]
    {
        imp::start_macos()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ScreenOcrStart::not_started(ScreenOcrNotStarted::Unsupported)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_capture_permission() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn screen_capture_allowed() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn ax_window_spi_is_linked_and_fails_for_windowless_elements() {
        // 真实动态调用 _AXUIElementGetWindow（非仅编译）：系统级元素不属于任何
        // 窗口，SPI 必须返回失败——链接缺失会在此炸链接，签名错误会崩或返回垃圾。
        assert!(crate::host_document::screen_ocr_ax_spi_negative_control());
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn ax_window_id_maps_into_the_cg_window_list_of_the_focused_app() {
        // 只读验证 _AXUIElementGetWindow：焦点元素的窗口 CGWindowID 必须出现在
        // CGWindowList 同一 pid 的窗口集合里。全程只取身份元数据，不读任何
        // 文字、不截屏。测试进程无辅助功能权限时显式留痕并跳过（映射验证
        // 本体不 silently pass——skip 信息会出现在测试日志里）。
        let Some((pid, ax_window_id)) = crate::host_document::screen_ocr_focused_window_identity()
        else {
            eprintln!("[screen-ocr-test] SKIP: focused AX element unavailable");
            return;
        };
        let windows = imp::list_onscreen_windows_for_tests()
            .expect("window list should be readable without permissions");
        let owned: Vec<u32> = windows
            .iter()
            .filter(|window| window.owner_pid == pid)
            .map(|window| window.id)
            .collect();
        assert!(
            owned.contains(&ax_window_id),
            "AX window id {ax_window_id} not in CG window list of pid {pid}: {owned:?}"
        );
    }
    #[test]
    fn frontmost_window_picks_the_topmost_normal_layer_window_of_the_frozen_app() {
        let windows = [
            WindowInfo {
                id: 101,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 102,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 900,
                owner_pid: 42,
                layer: 3,
            },
            WindowInfo {
                id: 201,
                owner_pid: 7,
                layer: 0,
            },
        ];
        assert_eq!(choose_frontmost_window(&windows, 42), Some(101));
        // 同 app 切到另一窗口：最前窗口变成 102，与冻结的 101 不符 → 复核会拒绝。
        assert_eq!(choose_frontmost_window(&windows, 42), Some(101));
        let switched = [
            WindowInfo {
                id: 102,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 101,
                owner_pid: 42,
                layer: 0,
            },
        ];
        assert_eq!(choose_frontmost_window(&switched, 42), Some(102));
        // 前台 app 只有浮层/菜单层窗口 → 无法确认普通窗口目标 → fail closed。
        assert_eq!(choose_frontmost_window(&windows, 9), None);
        assert_eq!(
            choose_frontmost_window(&windows, 6),
            None,
            "别家 app 的窗口不选"
        );
    }

    #[test]
    fn pick_frozen_window_prefers_the_focused_window_over_a_popup_on_top() {
        // 浏览器地址栏联想下拉压在主窗口上面：层序最前是弹窗（101），焦点在
        // 主窗口（102）——必须冻结主窗口，并标记为焦点冻结。
        let windows = [
            WindowInfo {
                id: 101,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 102,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 201,
                owner_pid: 7,
                layer: 0,
            },
        ];
        assert_eq!(pick_frozen_window(&windows, 42, Some(102)), Some((102, true)));
        // 焦点窗口不在列表里（AX 与 CG 列表对不上）→ 退回层序最前。
        assert_eq!(pick_frozen_window(&windows, 42, Some(999)), Some((101, false)));
        // 焦点窗口属于别的 pid → 不得接受，退回层序最前。
        assert_eq!(pick_frozen_window(&windows, 42, Some(201)), Some((101, false)));
        // 读不到焦点 → 退回层序最前。
        assert_eq!(pick_frozen_window(&windows, 42, None), Some((101, false)));
        // 该 app 只有浮层窗口 → 焦点窗口再像样也不选，fail closed。
        let floating_only = [WindowInfo {
            id: 900,
            owner_pid: 42,
            layer: 3,
        }];
        assert_eq!(pick_frozen_window(&floating_only, 42, Some(900)), None);
    }

    #[test]
    fn frozen_window_recheck_matches_the_freeze_mode() {
        // 弹窗（101）仍压在冻结的焦点窗口（102）上面。
        let windows = [
            WindowInfo {
                id: 101,
                owner_pid: 42,
                layer: 0,
            },
            WindowInfo {
                id: 102,
                owner_pid: 42,
                layer: 0,
            },
        ];
        // 焦点冻结：窗口仍在屏且属该 app 即有效（层序最前是弹窗无妨）。
        assert!(frozen_window_still_valid(&windows, 42, 102, true));
        // 层序冻结：最前普通层窗口换了即失效。
        assert!(!frozen_window_still_valid(&windows, 42, 102, false));
        assert!(frozen_window_still_valid(&windows, 42, 101, false));
        // 两种模式下：窗口消失或易主都失效。
        assert!(!frozen_window_still_valid(&windows, 42, 999, true));
        assert!(!frozen_window_still_valid(&windows, 7, 102, true));
    }

    #[test]
    fn relaxed_flag_is_set_before_the_result_and_readable_afterwards() {
        let slot = ScreenOcrSlotImpl::default();
        assert!(!ScreenOcrSlot::focus_gate_relaxed(&slot), "默认不得标记降级");
        // 观测线程的顺序约定：先立标记再交正文，core 收正文时读到的才是本次的。
        slot.mark_focus_gate_relaxed();
        slot.complete("正文".to_string());
        assert!(ScreenOcrSlot::focus_gate_relaxed(&slot));
        assert_eq!(ScreenOcrSlot::try_take(&slot).as_deref(), Some("正文"));
    }

    #[test]
    fn slot_yields_a_ready_result_exactly_once() {
        let slot = ScreenOcrSlotImpl::default();
        slot.complete("line 1\nline 2".to_string());
        assert_eq!(
            ScreenOcrSlot::try_take(&slot),
            Some("line 1\nline 2".to_string())
        );
        assert_eq!(ScreenOcrSlot::try_take(&slot), None);
    }

    #[test]
    fn a_pending_probe_terminates_the_slot() {
        let slot = ScreenOcrSlotImpl::default();
        assert_eq!(ScreenOcrSlot::try_take(&slot), None, "未就绪探测");
        slot.complete("late".to_string());
        assert_eq!(
            ScreenOcrSlot::try_take(&slot),
            None,
            "探测过未就绪的槽位已终结，晚到结果不得翻回 Ready"
        );
        assert!(!slot.alive(), "观测线程据 alive() 停止后续截屏");
    }

    #[test]
    fn slot_discard_beats_a_late_result_and_a_ready_result() {
        let slot = ScreenOcrSlotImpl::default();
        ScreenOcrSlot::discard(&slot);
        slot.complete("late".to_string());
        assert_eq!(ScreenOcrSlot::try_take(&slot), None);

        let ready = ScreenOcrSlotImpl::default();
        ready.complete("ready then cancelled".to_string());
        ScreenOcrSlot::discard(&ready);
        assert_eq!(ScreenOcrSlot::try_take(&ready), None);
        assert!(!ready.alive());
    }

    #[test]
    fn deadline_returns_the_result_of_fast_work() {
        let result = run_with_deadline(Duration::from_secs(2), || Ok("ok".to_string()));
        assert_eq!(result.as_deref(), Ok("ok"));
    }

    #[test]
    fn slot_finish_records_a_lightweight_outcome_once() {
        let slot = ScreenOcrSlotImpl::default();
        slot.finish(ScreenOcrOutcome::PermissionDenied);
        // 终态原因可被诊断读走，但正文路径拿不到任何文本。
        assert_eq!(
            ScreenOcrSlot::outcome(&slot),
            Some(ScreenOcrOutcome::PermissionDenied)
        );
        assert_eq!(ScreenOcrSlot::try_take(&slot), None);
        // finish 不覆盖既有终态，也不得让晚到的 complete 复活。
        slot.finish(ScreenOcrOutcome::Timeout);
        slot.complete("late".to_string());
        assert_eq!(
            ScreenOcrSlot::outcome(&slot),
            Some(ScreenOcrOutcome::PermissionDenied)
        );
        assert!(!slot.alive());
    }

    #[test]
    fn pending_probe_reports_no_outcome_until_terminal() {
        let slot = ScreenOcrSlotImpl::default();
        assert_eq!(ScreenOcrSlot::outcome(&slot), None, "Pending 没有终态原因");
        assert_eq!(ScreenOcrSlot::try_take(&slot), None);
        // 未就绪探测后 outcome 仍为 None：core 语义是 NotReady（不是某个失败）。
        assert_eq!(ScreenOcrSlot::outcome(&slot), None);
        ScreenOcrSlot::discard(&slot);
        assert_eq!(
            ScreenOcrSlot::outcome(&slot),
            Some(ScreenOcrOutcome::Discarded)
        );
    }

    #[test]
    fn deadline_drops_work_that_exceeds_it() {
        let result = run_with_deadline(Duration::from_millis(80), || {
            std::thread::sleep(Duration::from_millis(600));
            Ok("too slow".to_string())
        });
        assert_eq!(result.err(), Some(ScreenOcrOutcome::Timeout));
    }

    #[test]
    fn inflight_guard_caps_concurrency_and_releases_on_drop() {
        let first = InflightGuard::try_acquire();
        let second = InflightGuard::try_acquire();
        assert!(first.is_some() && second.is_some());
        assert!(InflightGuard::try_acquire().is_none(), "超过上限必须拒绝");
        drop(second);
        assert!(InflightGuard::try_acquire().is_some());
    }

    // ── 真实 Vision OCR：合成图（内存位图 + CoreText 画字），不碰用户屏幕 ──

    /// 最小 CoreText FFI：只声明画测试字必需的三个入口（仓库无 core-text 依赖，
    /// 沿 host_document/macos.rs 的手写 FFI 先例）。font 由 CFType RAII 包装持有，
    /// 不手动 CFRelease；ct_line 是裸指针（Create 规则），手动配对释放。
    #[cfg(target_os = "macos")]
    mod text_drawing {
        use core_foundation::attributed_string::CFMutableAttributedString;
        use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
        use core_foundation::string::CFString;
        use core_graphics::context::CGContext;
        use core_graphics::geometry::{CGAffineTransform, CGPoint, CGRect, CGSize};
        use foreign_types::ForeignType as _;

        #[link(name = "CoreText", kind = "framework")]
        extern "C" {
            static kCTFontAttributeName: core_foundation::string::CFStringRef;
            fn CTFontCreateWithName(
                name: core_foundation::string::CFStringRef,
                size: core_graphics::base::CGFloat,
                matrix: *const CGAffineTransform,
            ) -> CFTypeRef;
            fn CTLineCreateWithAttributedString(
                string: core_foundation::attributed_string::CFAttributedStringRef,
            ) -> CFTypeRef;
            fn CTLineDraw(line: CFTypeRef, context: *mut std::ffi::c_void);
        }

        /// 把 lines 以黑色 44pt 画到一张白色内存位图上（左下角原点，逐行抬高）。
        /// 全程内存，不触碰用户屏幕，也不需要任何系统权限。
        pub(crate) fn render_text_image(lines: &[&str]) -> Option<core_graphics::image::CGImage> {
            let width = 1200usize;
            let height = 160 * lines.len() + 80;
            let space = core_graphics::color_space::CGColorSpace::create_device_rgb();
            let context = CGContext::create_bitmap_context(
                None,
                width,
                height,
                8,
                0,
                &space,
                core_graphics::base::kCGImageAlphaPremultipliedLast,
            );
            unsafe {
                context.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
                context.fill_rect(CGRect::new(
                    &CGPoint::new(0.0, 0.0),
                    &CGSize::new(width as f64, height as f64),
                ));
                context.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
                let font_name = CFString::new("Helvetica");
                // CTFontCreateWithName 遵循 Create 命名约定（+1），RAII 接管。
                let font = CFType::wrap_under_create_rule(CTFontCreateWithName(
                    font_name.as_concrete_TypeRef(),
                    44.0,
                    std::ptr::null(),
                ));
                for (index, line) in lines.iter().enumerate() {
                    let text = CFString::new(line);
                    let mut attributed = CFMutableAttributedString::new();
                    attributed.replace_str(&text, CFRange::init(0, 0));
                    attributed.set_attribute(
                        CFRange::init(0, line.chars().count() as isize),
                        kCTFontAttributeName,
                        &font,
                    );
                    let ct_line =
                        CTLineCreateWithAttributedString(attributed.as_concrete_TypeRef());
                    if ct_line.is_null() {
                        return None;
                    }
                    context.set_text_position(40.0, 60.0 + 160.0 * index as f64);
                    CTLineDraw(ct_line, context.as_ptr() as *mut std::ffi::c_void);
                    core_foundation::base::CFRelease(ct_line);
                }
            }
            context.create_image()
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn frontmost_identity_read_runs_a_real_readonly_msg_send() {
        // 真实只读调用：验证 processIdentifier 按 pid_t（i32）收发的编码正确性。
        // 反复跑并在 pool 中回收，若引用/编码有错会在此崩溃。
        for _ in 0..8 {
            let outcome = objc2::rc::autoreleasepool(|_| imp_frontmost());
            let (pid, bundle) = outcome.expect("cargo test 运行时必有前台应用");
            assert!(pid > 0);
            assert!(!bundle.is_empty());
        }
    }

    #[cfg(target_os = "macos")]
    fn imp_frontmost() -> Option<(i32, String)> {
        imp::frontmost_pid_and_bundle_for_tests()
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn encode_png_round_trips_to_a_decodable_png() {
        let image = text_drawing::render_text_image(&["OpenLess PNG 编码测试"])
            .expect("synthetic bitmap should render");
        let png = imp::encode_png_for_tests(&image).expect("png encode should succeed");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG magic bytes");
        assert!(png.len() > 500, "png too small: {}", png.len());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_rapidocr_worker_recognizes_a_synthetic_bilingual_image() {
        // 真实拉起打包运行时里的 ocr_worker.py（隔离模式 + socket 拒绝守卫），
        // 识别合成图。runtime 是正常构建输入（AGENTS.md），不存在时显式留痕跳过。
        if crate::asr::local::qwen3_mlx::resolve_ocr_worker_locations().is_err() {
            eprintln!("[screen-ocr-test] SKIP: bundled OCR runtime not found");
            return;
        }
        let image = text_drawing::render_text_image(&[
            "OpenLess Local OCR 12345",
            "屏幕识别测试专用行",
        ])
        .expect("synthetic bitmap should render");
        let png = imp::encode_png_for_tests(&image).expect("png encode should succeed");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let text =
            crate::ocr_engine::ocr_png(&png, deadline).expect("worker should return text");
        let lowered = text.to_lowercase();
        assert!(lowered.contains("12345"), "got: {text:?}");
        assert!(lowered.contains("openless"), "got: {text:?}");
        assert!(
            ["屏", "识", "测"].iter().any(|c| text.contains(*c)),
            "got: {text:?}"
        );
    }
}
