//! macOS 屏幕 OCR 后台观测（本地 Vision，纯内存）。
//!
//! 听写触发那一刻 `start` 在调用线程上冻结前台目标（pid + window id），随后把
//! 截图与 OCR 全部搬到本模块自己的线程；录音 start 绝不为 OCR 等待。结果经
//! `openless_core::ports::ScreenOcrSlot` 在润色输入冻结点被非阻塞收取。
//!
//! ## 安全模型（全部绑定「冻结那一刻」的目标，后台只做复核、不做替换）
//!
//! 冻结时：前台 pid/bundle + Secure Input + bundle 黑名单（复用
//! `crate::host_document` 的闸门常量）+ 该 pid 最前面的普通层窗口 id；
//! 我们自己的 pid（胶囊/弹出层）永不截取。
//!
//! 后台复核（任一不符即拒绝截图，绝不退整屏）：
//! 1. 该 pid 的**最前**普通层窗口仍恰好是冻结的 window id——同 app 切到别的
//!    窗口同样拒绝（AX 密码闸门只认焦点，不能替另一个窗口背书）；
//! 2. 当前前台 pid 仍是冻结 pid（用户切走 app = 放弃本次观测）；
//! 3. `crate::host_document::screen_ocr_focus_gate`：焦点元素通过 Secure Input /
//!    敏感应用 / 密码框闸门，元素归属 pid == 冻结 pid，**且焦点所在 AX 窗口的
//!    _AXWindowID == 冻结 window id**；读不到焦点/窗口 id（fail closed）同样
//!    拒绝——所以本功能实际同时需要「屏幕录制」与「辅助功能」权限；
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

use std::sync::atomic::{AtomicUsize, Ordering};
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
}

impl Default for ScreenOcrSlotImpl {
    fn default() -> Self {
        Self {
            state: Mutex::new(SlotState::Pending),
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
        choose_frontmost_window, run_with_deadline, InflightGuard, ScreenOcrSlotImpl, WindowInfo,
        SCREEN_OCR_DEADLINE,
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
    /// 超大 = 目标状态异常，宁可跳过也别让 Vision 吃满 5 秒预算。
    const MAX_WINDOW_PIXELS: usize = 40_000_000;

    // initWithCGImage:options: 的 CGImageRef 参数编码是 ^{CGImage}，objc2 的
    // msg_send! 校验表达不了；这一条调用走固定签名的 objc_msgSend FFI。
    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_msgSend(
            receiver: *mut AnyObject,
            sel: objc2::runtime::Sel,
            image: *mut c_void,
            options: *mut AnyObject,
        ) -> *mut AnyObject;
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

    /// 本地 Vision OCR（zh-Hans + en-US，accurate，语言纠正）。objc2 只用
    /// msg_send/class（与 focus_target.rs 同模式）；NSArray/NSString 经 core-foundation
    /// 的 toll-free bridging 构造与读取。**引用计数约定**：request/handler 出自
    /// alloc/init（+1，手动 release 配对）；languages/empty_options/results 由 RAII
    /// wrapper（Drop = CFRelease）持有，不做手动 release；NSError 出参按约定是
    /// autoreleased，**绝不 release**。只在 OCR 工作线程（autoreleasepool 内）调用。
    unsafe fn recognize_text(image: &CGImage) -> Option<String> {
        use core_foundation::array::CFArray;

        let zh = CFString::new("zh-Hans");
        let en = CFString::new("en-US");
        let languages = CFArray::from_CFTypes(&[zh, en]);

        let request: *mut AnyObject = msg_send![class!(VNRecognizeTextRequest), alloc];
        let request: *mut AnyObject = msg_send![request, init];
        if request.is_null() {
            return None;
        }
        let _: () = msg_send![
            request,
            setRecognitionLanguages: languages.as_concrete_TypeRef() as *mut AnyObject
        ];
        // VNRequestTextRecognitionLevelAccurate == 0
        let _: () = msg_send![request, setRecognitionLevel: 0isize];
        let _: () = msg_send![request, setUsesLanguageCorrection: true];

        let empty_options = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]);
        let handler: *mut AnyObject = msg_send![class!(VNImageRequestHandler), alloc];
        let handler: *mut AnyObject = objc_msgSend(
            handler,
            objc2::sel!(initWithCGImage:options:),
            image.as_ptr() as *mut c_void,
            empty_options.as_concrete_TypeRef() as *mut AnyObject,
        );
        if handler.is_null() {
            let _: () = msg_send![request, release];
            return None;
        }

        let request_cf = CFType::wrap_under_get_rule(request as *const c_void);
        let requests = CFArray::from_CFTypes(&[request_cf]);
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![
            handler,
            performRequests: requests.as_concrete_TypeRef() as *mut AnyObject,
            error: &mut error as *mut *mut AnyObject
        ];
        let _: () = msg_send![handler, release];
        if !ok || !error.is_null() {
            // NSError 出参是 autoreleased（pool 会在收尾时回收），这里绝不能 release。
            log::info!("[screen-ocr] vision performRequests failed");
            let _: () = msg_send![request, release];
            return None;
        }

        // results getter 返回非持有引用（autoreleased/内部集合），wrap_under_get_rule
        // 取得自己的 +1，Drop 时配对释放——不手动 release。
        let results: *mut AnyObject = msg_send![request, results];
        if results.is_null() {
            let _: () = msg_send![request, release];
            return None;
        }
        let results_array = CFArray::<CFType>::wrap_under_get_rule(results as *const _);
        let mut lines = Vec::with_capacity(results_array.len() as usize);
        for index in 0..results_array.len() {
            let observation: *mut AnyObject = msg_send![results, objectAtIndex: index as usize];
            if observation.is_null() {
                continue;
            }
            let candidates: *mut AnyObject = msg_send![observation, topCandidates: 1usize];
            if candidates.is_null() {
                continue;
            }
            let top: *mut AnyObject = msg_send![candidates, firstObject];
            if top.is_null() {
                continue;
            }
            let string: *mut AnyObject = msg_send![top, string];
            if string.is_null() {
                continue;
            }
            lines.push(CFString::wrap_under_get_rule(string as *const _).to_string());
        }
        let _: () = msg_send![request, release];
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// 后台复核 + 截图 + OCR。复核全部绑定冻结的目标，并在**截图前与 OCR 前**
    /// 各检查一次槽位与截止时间——cancel/新会话/开关关闭/润色已冻结之后，
    /// 这里不会再产生任何截屏。任一复核不符直接放弃，绝不为「此刻的前台」
    /// 截一张替冻结窗口背书的图。
    fn observe(
        window_id: u32,
        frozen_pid: i32,
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
        // 同 app 切换窗口也拒绝：该 app 的最前普通层窗口必须仍是冻结的那一个。
        if choose_frontmost_window(&windows, frozen_pid) != Some(window_id) {
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
        if let Err(reason) = crate::host_document::screen_ocr_focus_gate(frozen_pid, window_id) {
            return bail(reason, focus_gate_outcome(reason));
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
        unsafe { recognize_text(&image) }.ok_or(ScreenOcrOutcome::Failed)
    }

    /// 焦点闸门拒绝原因 → 轻量结局。闸门 reason 是 &'static str（见
    /// host_document::macos::ocr_focus_gate）：归属/窗口漂移算 TargetChanged，
    /// Secure Input/密码框/黑名单应用算 SensitiveBlocked，AX 不可达（fail
    /// closed，含辅助功能权限缺失）算 Failed。
    fn focus_gate_outcome(reason: &str) -> ScreenOcrOutcome {
        if reason.contains("belongs to the frozen app") || reason.contains("frozen window") {
            ScreenOcrOutcome::TargetChanged
        } else if matches!(reason, "secure_input" | "secure_text_field" | "blocked_app") {
            ScreenOcrOutcome::SensitiveBlocked
        } else {
            ScreenOcrOutcome::Failed
        }
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
        let (pid, window_id) = match frozen {
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
                        observe(window_id, pid, deadline, &work_slot)
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
    pub(super) unsafe fn recognize_text_for_tests(image: &CGImage) -> Option<String> {
        recognize_text(image)
    }

    /// 调用线程上的目标冻结（毫秒级，无 AX 调用）。返回 (pid, window_id)；
    /// 拒绝原因以轻量枚举上抛（诊断要能说清「为什么没有这段上下文」）。
    fn freeze_target() -> Result<(i32, u32), ScreenOcrNotStarted> {
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
        let Some(window_id) = choose_frontmost_window(&windows, pid) else {
            log::info!("[screen-ocr] skip: no normal-layer window for the frozen app");
            return Err(ScreenOcrNotStarted::WindowUnavailable);
        };
        Ok((pid, window_id))
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
    fn real_vision_ocr_recognizes_a_synthetic_bilingual_image() {
        // 在真实 autoreleasepool 中反复执行完整识别：任何多释放/漏池的引用错误
        // 都会在多轮迭代里以崩溃或僵尸对象形式暴露。
        for round in 0..8 {
            let recognized = objc2::rc::autoreleasepool(|_| {
                let image = text_drawing::render_text_image(&[
                    "OpenLess Local OCR 12345",
                    "屏幕识别测试专用行",
                ])
                .expect("synthetic bitmap should render");
                unsafe { imp::recognize_text_for_tests(&image) }
            });
            let text = recognized.expect("vision should return text");
            let lowered = text.to_lowercase();
            assert!(lowered.contains("12345"), "round {round}; got: {text:?}");
            assert!(lowered.contains("openless"), "round {round}; got: {text:?}");
            assert!(
                ["屏", "识", "测"].iter().any(|c| text.contains(*c)),
                "round {round}; got: {text:?}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_vision_ocr_returns_none_for_a_blank_image() {
        for _ in 0..4 {
            let recognized = objc2::rc::autoreleasepool(|_| {
                let space = core_graphics::color_space::CGColorSpace::create_device_rgb();
                let context = core_graphics::context::CGContext::create_bitmap_context(
                    None,
                    200,
                    100,
                    8,
                    0,
                    &space,
                    core_graphics::base::kCGImageAlphaPremultipliedLast,
                );
                context.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
                context.fill_rect(core_graphics::geometry::CGRect::new(
                    &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                    &core_graphics::geometry::CGSize::new(200.0, 100.0),
                ));
                let image = context.create_image().expect("blank bitmap");
                unsafe { imp::recognize_text_for_tests(&image) }
            });
            assert_eq!(recognized, None);
        }
    }
}
