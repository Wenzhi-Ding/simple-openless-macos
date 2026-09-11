//! Shared value types used by every OpenLess host.

use serde::{Deserialize, Serialize};

pub use crate::types::HistorySource;

/// 模拟粘贴时实际按下的快捷键。macOS 走 AX 直写 / Cmd+V，本枚举只在
/// Windows / Linux 的 simulate_paste 路径生效。详见 issue #360：kitty 等
/// Linux 终端只接受 Ctrl+Shift+V，硬编码 Ctrl+V 会被吞掉，听写文本只剩
/// 在剪贴板里。默认 `CtrlV` 与历史行为一致；用户在 Settings 里改成
/// `CtrlShiftV`（kitty/alacritty/wezterm/gnome-terminal/foot/...）或
/// `ShiftInsert`（xterm/urxvt）后，simulate_paste 用对应组合。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PasteShortcut {
    #[default]
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
}

/// macOS 逐字上屏时换行符怎么发。仅流式插入路径生效。
///
/// 默认 `Auto`：按会话开始时冻结的前台应用选择。Terminal/TUI 使用 U+000A，
/// 其它和未知应用安全回退为 Shift+Return。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum MacosNewlineMode {
    /// 按会话开始时捕获的前台应用自动选择。
    #[default]
    Auto,
    /// Shift+Return：聊天框软换行，不发送。
    ShiftReturn,
    /// U+000A：Terminal/TUI 中等价于 Ctrl+J 软换行。
    LineFeed,
    /// Return：聊天框里等于发送。
    Return,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

pub use crate::types::HistoryInsertStatus as InsertStatus;

/// 前台应用标签拆分结果：人读的应用名 +（macOS 的）bundle id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontApp {
    pub name: Option<String>,
    pub bundle_id: Option<String>,
}

/// 把 `capture_frontmost_app()` 的显示串拆成 `FrontApp { name, bundle_id }`。
///
/// macOS 那边拼的是 `"Claude (com.anthropic.claudefordesktop)"`；Windows 拿的是窗口
/// 标题，没有 bundle id。历史条目有 `app_name` / `app_bundle_id` 两个字段，拆开存
/// 才能让详情页只显示人读得懂的应用名，而不是把一长串 bundle id 也糊在正文里。
///
/// 只有 macOS 的标签才是 `"名称 (bundle.id)"` 格式；Windows 拿的是窗口标题，括号属于
/// 标题正文。调用方必须按平台传入 `is_macos`（生产路径统一走 `split_front_app_opt`），
/// 非 macOS 一律整串当应用名。认不出括号结构也整串当应用名 —— 宁可显示得啰嗦，
/// 也不要把窗口标题里的普通括号误当成 bundle id。
pub fn split_front_app_label(label: &str, is_macos: bool) -> FrontApp {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return FrontApp {
            name: None,
            bundle_id: None,
        };
    }
    if is_macos {
        if let Some(open) = trimmed.rfind(" (") {
            if trimmed.ends_with(')') {
                let name = trimmed[..open].trim();
                let bundle = trimmed[open + 2..trimmed.len() - 1].trim();
                // bundle id 必然是点分的反向域名。没有点的括号内容（"记事本 (未保存)"
                // 这类窗口标题）不是 bundle id，不能拆。
                if !name.is_empty() && bundle.contains('.') && !bundle.contains(' ') {
                    return FrontApp {
                        name: Some(name.to_string()),
                        bundle_id: Some(bundle.to_string()),
                    };
                }
            }
        }
    }
    FrontApp {
        name: Some(trimmed.to_string()),
        bundle_id: None,
    }
}

/// `split_front_app_label` 的 `Option` 便捷版，平台开关收敛在这一处：
/// 只有 macOS 的显示串才是 `"名称 (bundle.id)"`，其它平台（Windows 窗口标题、Linux）
/// 整串当应用名，bundle id 留空。
pub fn split_front_app_opt(label: Option<&str>) -> FrontApp {
    label
        .map(|l| split_front_app_label(l, cfg!(target_os = "macos")))
        .unwrap_or(FrontApp {
            name: None,
            bundle_id: None,
        })
}

/// 概览页活动统计的单日汇总（date = 本地日期 YYYY-MM-DD）。
///
/// 年度热力图只用 `count`；`chars` / `duration_ms` 供「近 7 天 / 近 30 天」的
/// 字数与时长指标使用——这两个指标此前从 `list_history()` 现算，会被历史 200 条
/// 上限截断（说得多的用户几天就把上周挤没了）。
pub use crate::activity::ActivityDay;

pub use crate::types::DictationSession;

pub use crate::types::DictionaryEntry;

/// 落字失败兜底卡片的内容。
///
/// 文本没能落到目标 app 时（焦点在上屏途中离开、Secure Input、插入失败），把**完整**
/// 的那段话连同复制入口摆到用户面前。此前这些场景唯一的兜底是悄悄写剪贴板 —— 既依赖
/// 一个默认可关的开关，用户也不知道文本在那儿。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InsertFallbackCardPayload {
    /// 完整文本。焦点中途离开时屏幕上只有半截，这里给的是整段。
    pub text: String,
    /// 为什么没落进去。**只进日志，不上屏** —— 卡片没有标题行。见
    /// `INSERT_FALLBACK_REASON_*`。
    pub reason: String,
    /// 本次卡片展示的代次。尺寸测量 IPC 必须回传它，防止旧卡片迟到的报告缩放新卡片。
    pub presentation_id: u64,
}

/// 逐字上屏打到一半断了（Secure Input 中途打开、合成按键被拒）。
pub const INSERT_FALLBACK_REASON_PARTIAL_STREAM: &str = "partialStream";
/// 插入没能完成（Secure Input、辅助功能掉权限、粘贴被拒等）。
pub const INSERT_FALLBACK_REASON_INSERT_FAILED: &str = "insertFailed";
fn default_true() -> bool {
    true
}

fn default_silence_auto_stop_seconds() -> f32 {
    3.0
}

#[derive(Debug, Clone, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserPreferences {
    pub hotkey: HotkeyBinding,
    pub dictation_hotkey: ShortcutBinding,
    pub launch_at_login: bool,
    pub show_capsule: bool,
    /// 录音胶囊样式：'siri' = 流光 Siri 光效版（默认）；'classic' = Openless 经典药丸版。
    /// 由 capsule:state 事件的 capsuleStyle 字段下发到胶囊 webview，下次录音即生效。
    #[serde(default)]
    pub capsule_style: CapsuleStyle,
    /// 录音期间临时静音系统输出，停止/取消/出错后恢复原静音状态。
    #[serde(default)]
    pub mute_during_recording: bool,
    /// 按下录音热键进入 recording 状态时，播放一段即时合成的提示音，提醒「已开始录音」。
    /// 默认开启；可在「录音与输入」设置里关闭。提示音由 capsule 窗口用 Web Audio API 合成，
    /// 不依赖 show_capsule —— 胶囊隐藏时仍会响。
    #[serde(default = "default_true")]
    pub audio_cue_on_record: bool,
    /// Toggle 模式「说完自动停止」（issue #860）：检测到语音后，连续静音达到
    /// `silence_auto_stop_seconds` 时自动停止并提交；一直没检测到语音则 10 秒后
    /// 自动取消。默认关闭，保持既有「按两次」行为；Push-to-talk 不受影响。
    #[serde(default)]
    pub silence_auto_stop_enabled: bool,
    /// 语音后的连续静音阈值（秒）。可选 1 / 1.5 / 2 / 3 / 4 / 5，默认 3。
    #[serde(default = "default_silence_auto_stop_seconds")]
    pub silence_auto_stop_seconds: f32,
    /// 录音输入设备名称。空字符串 = 使用系统默认麦克风。
    #[serde(default)]
    pub microphone_device_name: String,
    pub active_asr_provider: String, // "volcengine" | "apple-speech" | ...
    pub active_llm_provider: String, // "ark" | "openai" | ...
    /// LLM 思考模式开关。默认 false 以保持既有「尽量关闭思考」行为；
    /// Gemini 走原生 thinkingConfig，OpenAI-compatible 路径仅按 provider/channel
    /// 下发官方渠道级字段；OpenAI 官方渠道会跳过普通 chat 模型不支持的字段。详见 issue #402。
    #[serde(default)]
    pub llm_thinking_enabled: bool,
    /// 是否使用系统代理（issue #869）。默认 true 跟随系统代理，与历史行为一致；
    /// 关闭后所有 reqwest 请求直连（国内服务通常延迟更低），GitHub 登录、更新等
    /// 境外服务可能连不上。实时语音流（WebSocket）与 Less Computer 子进程不受此开关影响。
    #[serde(default = "default_true")]
    pub use_system_proxy: bool,
    /// Windows / Linux 的模拟粘贴键。macOS 走 AX 直写不受影响。详见 issue #360：
    /// kitty 等 Linux 终端不接受 Ctrl+V，只能配 Ctrl+Shift+V。默认 CtrlV 与历史
    /// 行为一致，不破坏既有用户。
    #[serde(default)]
    pub paste_shortcut: PasteShortcut,
    /// macOS 逐字上屏的换行模拟方式。
    #[serde(default)]
    pub macos_newline_mode: MacosNewlineMode,
    /// 用户的工作语言（多选，原生名）。会作为前提注入 LLM polish 的 system prompt 头部，
    /// 让模型知道该用户在哪些语言间工作。详见 issue #4。
    #[serde(default = "default_working_languages")]
    pub working_languages: Vec<String>,
    /// 自定义录音组合键。当 `hotkey.trigger == Custom` 时，coordinator 用
    /// `global-hotkey` crate 注册此组合键（支持 Toggle + Hold 模式）。
    /// `None` 且 trigger == Custom 表示用户选了自定义但还没录制。
    #[serde(default)]
    pub custom_combo_hotkey: Option<ComboBinding>,
    /// 「唤起 App」全局快捷键。`None` = 停用；`Some(...)` = 注册。默认 `Some(默认键)`。
    #[serde(default = "default_open_app_hotkey")]
    pub open_app_hotkey: Option<ShortcutBinding>,
    /// 本地 Qwen3-ASR 当前激活的模型 id（"qwen3-asr-0.6b" / "qwen3-asr-1.7b"）。
    /// 仅在 active_asr_provider 为 local-qwen3 / local-qwen3-mlx / local-qwen3-c 时有意义。
    #[serde(default = "default_local_asr_model")]
    pub local_asr_active_model: String,
    /// 本地 ASR 引擎在内存中的保留时长（秒）。0 = 说完话即释放；
    /// 较大值 = 上次使用后驻留 N 秒再释放；86400 = 一天 ≈ 永不释放。
    /// 默认 300（5 分钟）：兼顾连续听写不重加载、长时间不用释放 1.2GB+ RAM。
    #[serde(default = "default_local_asr_keep_loaded_secs")]
    pub local_asr_keep_loaded_secs: u32,
    /// 本地模型自定义父目录。空字符串 = 使用系统默认 app data 下的 `models/`。
    /// 非空时，实际模型根目录为 `<local_asr_models_base_dir>/OpenLess/models/`，
    /// 让用户选择一个普通磁盘目录即可隔离 OpenLess 模型文件。
    #[serde(default)]
    pub local_asr_models_base_dir: String,
    /// 历史记录保留天数。0 = 不按时间清理（仅受 200 条上限）。默认 7 天。
    /// 写入新条目时执行清理，避免后台轮询。
    #[serde(default = "default_history_retention_days")]
    pub history_retention_days: u32,
    /// 对话感知 polish 的上下文窗口（分钟）：把最近 N 分钟的转写 + 已润色文本
    /// 作为多轮上下文喂给 LLM，让代词 / 不完整句子能被正确解析。
    /// 0 = 关闭（每次润色独立单轮，跟历史行为一致）。默认 5 分钟。
    #[serde(default = "default_polish_context_window_minutes")]
    pub polish_context_window_minutes: u32,
    /// 启动时静默运行（不弹主窗口）。开机自启用户用得多——本来想看托盘
    /// 而不是被主窗口打扰。开关一开后所有启动路径都不弹窗（包括手动点击），
    /// 用户改用托盘菜单访问主窗口。默认 false 跟历史行为一致。
    #[serde(default)]
    pub start_minimized: bool,
    /// UI theme: follow OS, force light, or force dark. Frontend applies via data-ol-theme.
    #[serde(default)]
    pub theme_mode: ThemeMode,
    /// 流式输入：润色 SSE 一边到达一边逐字模拟键盘事件输出到当前焦点。开启后用户感知到
    /// 的处理时延显著降低（润色 LLM 第一个 token 即开始落字）。
    ///
    /// 平台原语：
    /// - macOS：CGEvent Unicode FFI；CJK / 日文 IME 会拦截，session 期间临时切到 ABC
    /// - Windows：SendInput Unicode（绕过 TSF）；不需要切输入法
    /// - Linux：通过 fcitx5 插件 commitString 直写或剪贴板回落。
    ///
    /// 限制：
    /// - 不再走剪贴板路径，对 secure input 框（密码框 / 1Password）静默拒绝
    /// - 仅 OpenAI-compatible provider 实装（v1）；Gemini / Codex provider 走原一次性
    ///   插入路径
    ///
    /// 默认 true（自 1.3.2-3 起）—— 流式落字感知延迟低，所有 fallback case 都已经接好，
    /// 让开箱即用就能体验。CJK IME / Codex / Gemini provider 自动回落到一次性路径，
    /// 用户无感。详见上面「限制」段。
    #[serde(default = "default_true")]
    pub streaming_insert: bool,
    /// issue #440 的一次性迁移标记。老版本会把默认 `streamingInsert:false`
    /// 写进 preferences.json，升级后仅看 bool 无法区分「老默认」和「用户手动关」。
    /// 缺少此标记的旧文件统一迁到 true；迁移后用户再关会带着标记保存，后续保留 false。
    #[serde(default)]
    pub streaming_insert_default_migrated: bool,
    /// 流式输入成功后是否把最终润色文本写回剪贴板。一次性路径天然走剪贴板，所以
    /// Cmd+V 可以重复粘贴；流式路径直接合成键盘事件、不动剪贴板，会让用户失去这层
    /// 兜底。开启后流式成功收尾时把 final text 写到系统剪贴板，跟一次性行为对齐。
    /// 默认 true（更接近用户习惯）。
    #[serde(default = "default_true")]
    pub streaming_insert_save_clipboard: bool,
    /// 是否把「用户正在写的那篇文档」中光标附近的原文送进 LLM 润色当上下文。
    ///
    /// **默认 false，且必须保持 false。** 开启后每次听写都会读取前台 app 的正文并把
    /// 其中一段发给 LLM 服务商——这是用户没有主动交给我们的数据，只能由用户显式选择。
    /// 关闭时 `host_document` 一次 AX 都不发，prompt 与本功能存在之前逐字节相同。
    ///
    /// 目前仅 macOS 有实现；Windows / Linux 开了也读不到，优雅降级为无上下文。
    /// 密码框 / Secure Input / 密码管理器 / 终端一律硬拦，与本开关无关。
    #[serde(default)]
    pub cursor_context_enabled: bool,
    /// 听写开始时是否对前台窗口做一次本地（Vision）屏幕 OCR，并把识别文字作为
    /// 不可信背景参考送进润色，用于消歧义与专名写法。与 cursor_context 相互独立。
    ///
    /// **默认 false，且必须保持 false。** 截图只发生在内存里、不落盘不进日志，
    /// 但这仍是用户没有主动交给我们的屏幕内容，只能由用户显式选择开启。
    /// 需要系统「屏幕录制」权限；无权限 / 敏感应用 / Secure Input 一律静默跳过，
    /// 绝不自动弹系统授权。仅 macOS 实现。关闭时润色 prompt 与本功能存在之前逐字节相同。
    #[serde(default)]
    pub screen_ocr_context_enabled: bool,
    /// 概览页是否显示「年度活动」热力图卡。默认 true；关闭只隐藏卡片，
    /// 活动计数照常记录（persistence/activity.rs），再打开时全年数据仍在。
    #[serde(default = "default_true")]
    pub show_overview_activity_heatmap: bool,
    /// 易读布局：小屏或大字号时强制同行控件换行，避免横向溢出与文字被压扁。默认 false。
    #[serde(default)]
    pub stacked_row_layout: bool,
    /// 保守排版：除首页、顶栏、底栏与胶囊窗外，内容区强制单列满宽。默认 false。
    #[serde(default)]
    pub conservative_layout: bool,
    /// 主窗口启动 + 后台每 60 分钟自动检查更新。默认 true。
    /// 关闭后仅 Settings 手动「检查更新」按钮可用。
    #[serde(default = "default_true")]
    pub auto_update_check: bool,
    /// 历史记录上限（条数）。`None` = 使用代码内 200 条硬上限；
    /// `Some(n)` 表示用户在 Settings 自定义了上限（5..=200 之间）。
    #[serde(default)]
    pub history_max_entries: Option<u32>,
    /// 是否为每次会话保留原始麦克风音频文件（wav）到 `recordings/` 目录，
    /// 用于排查 ASR 误识别 / 麦克风灵敏度问题。默认 false。开启会占磁盘空间，
    /// 受 `history_retention_days` 同样的清理策略约束。
    #[serde(default)]
    pub record_audio_for_debug: bool,
    /// `recordings/` 里保留的最近 wav 文件数（按 mtime 倒序保留最新的）。
    /// `None` = 跟随 `HISTORY_CAP` (200)；`Some(n)` 时 clamp 到 1..=200。
    /// 调用点：每次开新会话前裁旧。让用户在「文本历史保留 200 条但 wav 只留最近 5 条」
    /// 这种「文本档案多 + 录音不占盘」组合下精确控制。
    #[serde(default)]
    pub audio_recording_max_entries: Option<u32>,
}

fn default_local_asr_model() -> String {
    "qwen3-asr-0.6b".into()
}

fn default_history_retention_days() -> u32 {
    7
}

fn default_polish_context_window_minutes() -> u32 {
    5
}

fn default_local_asr_keep_loaded_secs() -> u32 {
    300
}

fn default_active_asr_provider() -> String {
    crate::local_asr_catalog::LOCAL_QWEN3_MLX_PROVIDER_ID.into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UserPreferencesWire {
    hotkey: HotkeyBinding,
    dictation_hotkey: Option<ShortcutBinding>,
    launch_at_login: bool,
    show_capsule: bool,
    #[serde(default)]
    capsule_style: CapsuleStyle,
    #[serde(default)]
    mute_during_recording: bool,
    #[serde(default = "default_true")]
    audio_cue_on_record: bool,
    #[serde(default)]
    silence_auto_stop_enabled: bool,
    #[serde(default = "default_silence_auto_stop_seconds")]
    silence_auto_stop_seconds: f32,
    #[serde(default)]
    microphone_device_name: String,
    active_asr_provider: String,
    active_llm_provider: String,
    #[serde(default)]
    llm_thinking_enabled: bool,
    #[serde(default = "default_true")]
    use_system_proxy: bool,
    #[serde(default)]
    paste_shortcut: PasteShortcut,
    #[serde(default)]
    macos_newline_mode: MacosNewlineMode,
    working_languages: Vec<String>,
    custom_combo_hotkey: Option<ComboBinding>,
    open_app_hotkey: Option<ShortcutBinding>,
    #[serde(default = "default_local_asr_model")]
    local_asr_active_model: String,
    #[serde(default = "default_local_asr_keep_loaded_secs")]
    local_asr_keep_loaded_secs: u32,
    #[serde(default)]
    local_asr_models_base_dir: String,
    #[serde(default = "default_history_retention_days")]
    history_retention_days: u32,
    #[serde(default = "default_polish_context_window_minutes")]
    polish_context_window_minutes: u32,
    #[serde(default)]
    start_minimized: bool,
    #[serde(default)]
    theme_mode: ThemeMode,
    #[serde(default = "default_true")]
    streaming_insert: bool,
    #[serde(default)]
    streaming_insert_default_migrated: bool,
    #[serde(default = "default_true")]
    streaming_insert_save_clipboard: bool,
    #[serde(default)]
    cursor_context_enabled: bool,
    #[serde(default)]
    screen_ocr_context_enabled: bool,
    #[serde(default = "default_true")]
    show_overview_activity_heatmap: bool,
    #[serde(default)]
    stacked_row_layout: bool,
    #[serde(default)]
    conservative_layout: bool,
    #[serde(default = "default_true")]
    auto_update_check: bool,
    #[serde(default)]
    history_max_entries: Option<u32>,
    #[serde(default)]
    record_audio_for_debug: bool,
    #[serde(default)]
    audio_recording_max_entries: Option<u32>,
}

impl Default for UserPreferencesWire {
    fn default() -> Self {
        let prefs = UserPreferences::default();
        Self {
            hotkey: prefs.hotkey,
            dictation_hotkey: None,
            launch_at_login: prefs.launch_at_login,
            show_capsule: prefs.show_capsule,
            capsule_style: prefs.capsule_style,
            mute_during_recording: prefs.mute_during_recording,
            audio_cue_on_record: prefs.audio_cue_on_record,
            silence_auto_stop_enabled: prefs.silence_auto_stop_enabled,
            silence_auto_stop_seconds: prefs.silence_auto_stop_seconds,
            microphone_device_name: prefs.microphone_device_name,
            active_asr_provider: prefs.active_asr_provider,
            active_llm_provider: prefs.active_llm_provider,
            llm_thinking_enabled: prefs.llm_thinking_enabled,
            use_system_proxy: prefs.use_system_proxy,
            paste_shortcut: prefs.paste_shortcut,
            macos_newline_mode: prefs.macos_newline_mode,
            working_languages: prefs.working_languages,
            custom_combo_hotkey: prefs.custom_combo_hotkey,
            open_app_hotkey: prefs.open_app_hotkey,
            local_asr_active_model: prefs.local_asr_active_model,
            // 新字段必须保持 None：旧配置反序列化时需要区分“字段缺失”和显式值。
            local_asr_keep_loaded_secs: prefs.local_asr_keep_loaded_secs,
            local_asr_models_base_dir: prefs.local_asr_models_base_dir,
            history_retention_days: prefs.history_retention_days,
            polish_context_window_minutes: prefs.polish_context_window_minutes,
            start_minimized: prefs.start_minimized,
            theme_mode: prefs.theme_mode,
            streaming_insert: prefs.streaming_insert,
            streaming_insert_default_migrated: prefs.streaming_insert_default_migrated,
            streaming_insert_save_clipboard: prefs.streaming_insert_save_clipboard,
            cursor_context_enabled: prefs.cursor_context_enabled,
            screen_ocr_context_enabled: prefs.screen_ocr_context_enabled,
            show_overview_activity_heatmap: prefs.show_overview_activity_heatmap,
            stacked_row_layout: prefs.stacked_row_layout,
            conservative_layout: prefs.conservative_layout,
            auto_update_check: prefs.auto_update_check,
            history_max_entries: prefs.history_max_entries,
            record_audio_for_debug: prefs.record_audio_for_debug,
            audio_recording_max_entries: prefs.audio_recording_max_entries,
        }
    }
}

impl<'de> Deserialize<'de> for UserPreferences {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = UserPreferencesWire::deserialize(deserializer)?;
        let dictation_hotkey = match wire.dictation_hotkey {
            Some(binding) => binding,
            None => default_dictation_hotkey_from_legacy(&wire.hotkey, &wire.custom_combo_hotkey)
                .map_err(serde::de::Error::custom)?,
        };
        let streaming_insert_default_migrated = wire.streaming_insert_default_migrated;
        let streaming_insert = if streaming_insert_default_migrated {
            wire.streaming_insert
        } else {
            true
        };
        let local_asr_active_model = wire.local_asr_active_model;

        Ok(Self {
            hotkey: wire.hotkey,
            dictation_hotkey,
            launch_at_login: wire.launch_at_login,
            show_capsule: wire.show_capsule,
            capsule_style: wire.capsule_style,
            mute_during_recording: wire.mute_during_recording,
            audio_cue_on_record: wire.audio_cue_on_record,
            silence_auto_stop_enabled: wire.silence_auto_stop_enabled,
            silence_auto_stop_seconds: wire.silence_auto_stop_seconds,
            microphone_device_name: wire.microphone_device_name,
            active_asr_provider: wire.active_asr_provider,
            active_llm_provider: wire.active_llm_provider,
            llm_thinking_enabled: wire.llm_thinking_enabled,
            use_system_proxy: wire.use_system_proxy,
            paste_shortcut: wire.paste_shortcut,
            macos_newline_mode: wire.macos_newline_mode,
            working_languages: wire.working_languages,
            custom_combo_hotkey: wire.custom_combo_hotkey,
            open_app_hotkey: wire.open_app_hotkey,
            local_asr_active_model,
            local_asr_keep_loaded_secs: wire.local_asr_keep_loaded_secs,
            local_asr_models_base_dir: wire.local_asr_models_base_dir,
            history_retention_days: wire.history_retention_days,
            polish_context_window_minutes: wire.polish_context_window_minutes,
            start_minimized: wire.start_minimized,
            theme_mode: wire.theme_mode,
            streaming_insert,
            streaming_insert_default_migrated: true,
            streaming_insert_save_clipboard: wire.streaming_insert_save_clipboard,
            cursor_context_enabled: wire.cursor_context_enabled,
            screen_ocr_context_enabled: wire.screen_ocr_context_enabled,
            show_overview_activity_heatmap: wire.show_overview_activity_heatmap,
            stacked_row_layout: wire.stacked_row_layout,
            conservative_layout: wire.conservative_layout,
            auto_update_check: wire.auto_update_check,
            history_max_entries: wire.history_max_entries,
            record_audio_for_debug: wire.record_audio_for_debug,
            audio_recording_max_entries: wire.audio_recording_max_entries,
        })
    }
}

impl UserPreferences {
    /// 逐字段抢救一份无法严格反序列化的 preferences.json。
    ///
    /// 背景：`UserPreferencesWire` 容器级 `#[serde(default)]` 已能容忍「缺字段」
    /// （老文件读新版本）。真正会让整份解析失败、进而静默回落默认值（= 用户所有
    /// 设置一次性丢光）的，是「字段存在但值非法」——例如某次重构改了枚举变体名 /
    /// 字段类型，旧文件里的旧值在新版本里不再合法。这正是用户反馈「每次重装 app
    /// 之后热键等设置就读不到」的根因路径。
    ///
    /// 抢救策略：把 JSON 当作对象，先归一化已知 alias，再逐 key 试解析。因为 Wire 对
    /// 所有字段都有 default，单键对象 `{k: v}` 只有当 `v` 对字段 `k` 的类型非法时才会
    /// 失败——据此精确剔除坏字段，保留其余全部有效设置（热键、模型选择、风格等都能
    /// 活下来），最后再走一次正常反序列化。无法当作对象解析时才彻底回落默认。
    pub fn salvage_from_json_bytes(bytes: &[u8]) -> Self {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(bytes)
        else {
            return Self::default();
        };

        let mut cleaned = serde_json::Map::new();
        for (key, value) in map {
            if preference_field_is_valid(&key, &value) {
                cleaned.insert(key, value);
            } else {
                log::warn!("[prefs] salvage dropping unparseable field: {key}");
            }
        }

        match serde_json::from_value::<Self>(serde_json::Value::Object(cleaned.clone())) {
            Ok(prefs) => prefs,
            Err(err) => {
                if let Some(prefs) = salvage_without_incomplete_legacy_hotkey(cleaned) {
                    return prefs;
                }
                log::warn!(
                    "[prefs] salvage still failed after field filtering: {err}; using defaults"
                );
                Self::default()
            }
        }
    }
}

fn preference_field_is_valid(key: &str, value: &serde_json::Value) -> bool {
    let probe =
        serde_json::Value::Object(std::iter::once((key.to_string(), value.clone())).collect());
    serde_json::from_value::<UserPreferencesWire>(probe).is_ok()
}

fn salvage_without_incomplete_legacy_hotkey(
    mut map: serde_json::Map<String, serde_json::Value>,
) -> Option<UserPreferences> {
    let is_custom_legacy_hotkey = map
        .get("hotkey")
        .and_then(|value| value.get("trigger"))
        .and_then(serde_json::Value::as_str)
        == Some("custom");
    if !is_custom_legacy_hotkey {
        return None;
    }

    let has_dictation_hotkey = map
        .get("dictationHotkey")
        .and_then(|value| serde_json::from_value::<Option<ShortcutBinding>>(value.clone()).ok())
        .flatten()
        .is_some();
    let has_custom_combo_hotkey = map
        .get("customComboHotkey")
        .and_then(|value| serde_json::from_value::<Option<ComboBinding>>(value.clone()).ok())
        .flatten()
        .is_some();
    if has_dictation_hotkey || has_custom_combo_hotkey {
        return None;
    }

    map.remove("hotkey");
    serde_json::from_value::<UserPreferences>(serde_json::Value::Object(map)).ok()
}

fn default_open_app_hotkey() -> Option<ShortcutBinding> {
    Some(ShortcutBinding {
        primary: "O".into(),
        modifiers: default_app_shortcut_modifiers(),
    })
}

fn default_app_shortcut_modifiers() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        vec!["cmd".into(), "shift".into()]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec!["ctrl".into(), "shift".into()]
    }
}

fn default_dictation_hotkey_from_legacy(
    hotkey: &HotkeyBinding,
    custom_combo_hotkey: &Option<ComboBinding>,
) -> Result<ShortcutBinding, String> {
    if hotkey.trigger == HotkeyTrigger::Custom {
        if let Some(combo) = custom_combo_hotkey {
            return Ok(ShortcutBinding {
                primary: combo.primary.clone(),
                modifiers: combo.modifiers.clone(),
            });
        }
        return Err(
            "hotkey.trigger is custom but dictationHotkey/customComboHotkey is missing".into(),
        );
    }
    Ok(crate::shortcut_types::binding_from_legacy_trigger(
        hotkey.trigger,
    ))
}

fn default_working_languages() -> Vec<String> {
    vec!["简体中文".into()]
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            hotkey: HotkeyBinding::default(),
            dictation_hotkey: default_dictation_hotkey_from_legacy(
                &HotkeyBinding::default(),
                &None,
            )
            .expect("default legacy hotkey is not custom"),
            launch_at_login: false,
            show_capsule: true,
            capsule_style: CapsuleStyle::Siri,
            mute_during_recording: false,
            audio_cue_on_record: true,
            silence_auto_stop_enabled: false,
            silence_auto_stop_seconds: default_silence_auto_stop_seconds(),
            microphone_device_name: String::new(),
            active_asr_provider: default_active_asr_provider(),
            active_llm_provider: "local-llama".into(),
            llm_thinking_enabled: false,
            use_system_proxy: false,
            paste_shortcut: PasteShortcut::default(),
            macos_newline_mode: MacosNewlineMode::default(),
            working_languages: default_working_languages(),
            custom_combo_hotkey: None,
            open_app_hotkey: default_open_app_hotkey(),
            local_asr_active_model: default_local_asr_model(),
            local_asr_keep_loaded_secs: default_local_asr_keep_loaded_secs(),
            local_asr_models_base_dir: String::new(),
            history_retention_days: default_history_retention_days(),
            polish_context_window_minutes: default_polish_context_window_minutes(),
            start_minimized: false,
            theme_mode: ThemeMode::default(),
            streaming_insert: true,
            streaming_insert_default_migrated: true,
            streaming_insert_save_clipboard: true,
            cursor_context_enabled: false,
            screen_ocr_context_enabled: false,
            show_overview_activity_heatmap: true,
            stacked_row_layout: false,
            conservative_layout: false,
            auto_update_check: true,
            history_max_entries: None,
            record_audio_for_debug: false,
            audio_recording_max_entries: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutBinding {
    pub primary: String,
    pub modifiers: Vec<String>,
}

impl ShortcutBinding {
    pub fn display_label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let modifier_order = ["cmd", "ctrl", "alt", "shift", "super"];
        for tag in modifier_order {
            if self.modifiers.iter().any(|m| m.eq_ignore_ascii_case(tag)) {
                parts.push(modifier_display(tag).to_string());
            }
        }
        parts.push(display_primary(&self.primary));
        parts.join("+")
    }
}

/// 录音快捷键的自定义组合键绑定。结构与 `QaHotkeyBinding` 相同：
/// - `primary`：主键（如 `"D"`、`"Space"`、`"F1"`）。
/// - `modifiers`：修饰键集合，元素来自 `{"cmd","ctrl","alt","shift","super"}`。
///
/// 当 `HotkeyBinding.trigger == Custom` 时，coordinator 用 `global-hotkey` crate
/// 注册此组合键，而非 modifier-only 的 CGEventTap / WH_KEYBOARD_LL。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComboBinding {
    pub primary: String,
    pub modifiers: Vec<String>,
}

impl ComboBinding {
    pub fn display_label(&self) -> String {
        ShortcutBinding {
            primary: self.primary.clone(),
            modifiers: self.modifiers.clone(),
        }
        .display_label()
    }
}

fn modifier_display(tag: &str) -> &'static str {
    match tag {
        "cmd" => "Cmd",
        "ctrl" => "Ctrl",
        "alt" => "Option",
        "shift" => "Shift",
        "super" => "Super",
        _ => "",
    }
}

fn display_primary(primary: &str) -> String {
    let trimmed = primary.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }
    // 单个字母键归一为大写显示（"a" → "A"）；其余原样（如 ";"、"F1"）。
    if trimmed.chars().count() == 1 {
        let ch = trimmed.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return ch.to_ascii_uppercase().to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyTrigger {
    RightOption,
    LeftOption,
    RightControl,
    LeftControl,
    RightCommand,
    LeftCommand,
    LeftShift,
    RightShift,
    Fn,
    RightAlt, // Windows synonym for RightOption
    MediaPlayPause,
    Custom,
}

impl HotkeyTrigger {
    pub fn display_name(&self) -> &'static str {
        match self {
            HotkeyTrigger::RightOption => "右 Option",
            HotkeyTrigger::LeftOption => "左 Option",
            HotkeyTrigger::RightControl => "右 Control",
            HotkeyTrigger::LeftControl => "左 Control",
            HotkeyTrigger::RightCommand => "右 Command",
            HotkeyTrigger::LeftCommand => "左 Command",
            HotkeyTrigger::LeftShift => "左 Shift",
            HotkeyTrigger::RightShift => "右 Shift",
            HotkeyTrigger::Fn => "Fn (地球键)",
            HotkeyTrigger::RightAlt => "右 Alt",
            HotkeyTrigger::MediaPlayPause => "⏯ Media 播放/暂停",
            HotkeyTrigger::Custom => "自定义组合键",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyMode {
    Toggle,
    Hold,
    DoubleClick,
    /// 自动识别：按下即开录；松手时按「按住时长」决定语义 —— 短按（< AUTO_HOLD_THRESHOLD）
    /// 当作 Toggle（锁存，保持录音，下次按下再停），长按当作 Hold（松手即停）。
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyAdapterKind {
    MacEventTap,
    WindowsLowLevel,
    Fcitx5,
    /// No supported native hotkey adapter is available.
    Unavailable,
}

impl HotkeyAdapterKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            HotkeyAdapterKind::MacEventTap => "macOS Event Tap",
            HotkeyAdapterKind::WindowsLowLevel => "Windows 低层键盘 hook",
            HotkeyAdapterKind::Fcitx5 => "fcitx5 输入法插件",
            HotkeyAdapterKind::Unavailable => "不可用",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyKey {
    pub code: String,
}

impl HotkeyKey {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct HotkeyBinding {
    pub trigger: HotkeyTrigger,
    pub mode: HotkeyMode,
    pub keys: Option<Vec<HotkeyKey>>,
}

impl HotkeyBinding {
    pub fn effective_codes(&self) -> Vec<String> {
        let Some(keys) = &self.keys else {
            let code = legacy_trigger_code(self.trigger);
            return if code.is_empty() {
                Vec::new()
            } else {
                vec![code.to_string()]
            };
        };
        keys.iter()
            .map(|key| key.code.trim().to_string())
            .filter(|code| !code.is_empty())
            .collect()
    }

    pub fn display_label(&self) -> String {
        let codes = self.effective_codes();
        if codes.is_empty() {
            return "未设置".to_string();
        }
        codes
            .iter()
            .map(|code| display_hotkey_code(code))
            .collect::<Vec<_>>()
            .join("+")
    }
}

fn legacy_trigger_code(trigger: HotkeyTrigger) -> &'static str {
    match trigger {
        HotkeyTrigger::RightOption | HotkeyTrigger::RightAlt => "AltRight",
        HotkeyTrigger::LeftOption => "AltLeft",
        HotkeyTrigger::RightControl => "ControlRight",
        HotkeyTrigger::LeftControl => "ControlLeft",
        HotkeyTrigger::RightCommand => "MetaRight",
        HotkeyTrigger::LeftCommand => "MetaLeft",
        HotkeyTrigger::LeftShift => "ShiftLeft",
        HotkeyTrigger::RightShift => "ShiftRight",
        HotkeyTrigger::Fn => "Fn",
        HotkeyTrigger::MediaPlayPause => "MediaPlayPause",
        HotkeyTrigger::Custom => "",
    }
}

fn display_hotkey_code(code: &str) -> String {
    let label = match code {
        "ControlLeft" => "左Ctrl",
        "ControlRight" => "右 Control",
        "AltLeft" => "左Alt",
        "AltRight" => "右Alt",
        "ShiftLeft" => "左Shift",
        "ShiftRight" => "右Shift",
        "MetaLeft" | "OSLeft" => "左Win",
        "MetaRight" | "OSRight" => "右Win",
        "Fn" => "Fn",
        "FnLock" => "FnLock",
        "CapsLock" => "CapsLock",
        "ScrollLock" => "ScrLock",
        "Pause" => "Pause",
        "PrintScreen" => "PrtSc",
        "Backspace" => "Backspace",
        "Tab" => "Tab",
        "Enter" => "Enter",
        "Space" => "Space",
        "Insert" => "Insert",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "PageUp",
        "PageDown" => "PageDown",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "NumpadAdd" => "Num+",
        "NumpadSubtract" => "Num-",
        "NumpadMultiply" => "Num*",
        "NumpadDivide" => "Num/",
        "NumpadDecimal" => "Num.",
        "NumpadEnter" => "NumEnter",
        "Mouse4" => "Mouse4",
        "Mouse5" => "Mouse5",
        "Backquote" => "`",
        "Minus" => "-",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Backslash" => "\\",
        "Semicolon" => ";",
        "Quote" => "'",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        _ => "",
    };
    if !label.is_empty() {
        return label.to_string();
    }
    if let Some(letter) = code.strip_prefix("Key") {
        if letter.len() == 1 {
            return letter.to_string();
        }
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        if digit.len() == 1 {
            return digit.to_string();
        }
    }
    if let Some(num) = code.strip_prefix("Numpad") {
        if num.len() == 1 && num.as_bytes()[0].is_ascii_digit() {
            return format!("Num{num}");
        }
    }
    code.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyCapability {
    pub adapter: HotkeyAdapterKind,
    pub available_triggers: Vec<HotkeyTrigger>,
    pub requires_accessibility_permission: bool,
    pub supports_modifier_only_trigger: bool,
    pub supports_side_specific_modifiers: bool,
    pub explicit_fallback_available: bool,
    pub status_hint: Option<String>,
}

impl HotkeyCapability {
    pub fn current() -> Self {
        Self {
            adapter: HotkeyAdapterKind::MacEventTap,
            available_triggers: vec![
                HotkeyTrigger::RightOption,
                HotkeyTrigger::LeftOption,
                HotkeyTrigger::RightControl,
                HotkeyTrigger::LeftControl,
                HotkeyTrigger::RightCommand,
                HotkeyTrigger::LeftCommand,
                HotkeyTrigger::LeftShift,
                HotkeyTrigger::RightShift,
                HotkeyTrigger::Fn,
                HotkeyTrigger::Custom,
            ],
            requires_accessibility_permission: true,
            supports_modifier_only_trigger: true,
            supports_side_specific_modifiers: true,
            explicit_fallback_available: false,
            status_hint: Some("授权辅助功能后，通常需要完全退出并重新打开 OpenLess。".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyInstallError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for HotkeyInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub adapter: HotkeyAdapterKind,
    pub state: HotkeyStatusState,
    pub message: Option<String>,
    pub last_error: Option<HotkeyInstallError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub platform: String,
    pub supports_ime_input: bool,
    pub supports_overlay: bool,
    pub supports_desktop_hotkey: bool,
    pub supports_tray: bool,
    pub supports_local_asr: bool,
    pub supports_local_qwen3_mlx: bool,
    pub supports_in_app_dictation: bool,
    pub supports_auto_update: bool,
}

impl PlatformCapabilities {
    pub fn current() -> Self {
        Self {
            platform: "macos".to_string(),
            supports_ime_input: false,
            supports_overlay: true,
            supports_desktop_hotkey: true,
            supports_tray: true,
            supports_local_asr: true,
            supports_local_qwen3_mlx: true,
            supports_in_app_dictation: false,
            supports_auto_update: false,
        }
    }
}

impl Default for PlatformCapabilities {
    fn default() -> Self {
        Self {
            platform: "unknown".to_string(),
            supports_ime_input: false,
            supports_overlay: false,
            supports_desktop_hotkey: false,
            supports_tray: false,
            supports_local_asr: false,
            supports_local_qwen3_mlx: false,
            supports_in_app_dictation: false,
            supports_auto_update: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyStatusState {
    Starting,
    Installed,
    Failed,
}

impl Default for HotkeyStatus {
    fn default() -> Self {
        Self {
            adapter: HotkeyCapability::current().adapter,
            state: HotkeyStatusState::Starting,
            message: Some("正在安装全局快捷键监听".into()),
            last_error: None,
        }
    }
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        // 注意：keys 必须是 None，不能预填具体 code。
        //
        // 原因：HotkeyBinding 用 `#[serde(default)]` **结构级 default**——反序列化时
        // 整个 struct 先按 Default 填充再让 JSON 字段覆盖。如果这里 keys 预填了
        // Some([...])，那么旧 prefs 里只写 `{"trigger":"rightControl","mode":"toggle"}`
        // （不带 keys 字段）会被反序列化成 `{trigger=RightControl, keys=Some([默认值])}`
        // 即 trigger 跟 keys 完全不一致——effective_codes() 直接信任 keys，导致
        // 实际生效的快捷键跟用户当年选的 trigger 对不上。
        // 现在 keys=None 时 effective_codes() 走 legacy_trigger_code(trigger) 路径，
        // 跟 trigger 自动同步。
        Self {
            trigger: HotkeyTrigger::RightCommand,
            mode: HotkeyMode::Toggle,
            keys: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CapsuleState {
    Idle,
    Recording,
    Transcribing,
    Polishing,
    Done,
    Cancelled,
    Error,
}

/// 录音胶囊样式。由 UserPreferences.capsule_style 透传到 capsule:state payload，
/// 胶囊 webview 据此选择渲染流光 Siri 光效舞台还是经典药丸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CapsuleStyle {
    /// 流光 Siri 风格：SiriGL 光效舞台（默认）。
    #[default]
    Siri,
    /// Openless 默认风格：经典毛玻璃药丸（音量条 + 取消/确认按钮）。
    Classic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapsulePayload {
    pub state: CapsuleState,
    pub level: f32, // 0..1 RMS
    pub elapsed_ms: u64,
    pub message: Option<String>,
    pub inserted_chars: Option<u32>,
    /// 预备态：胶囊已经"乐观显示"出来（按下热键即弹出并播入场动画），但麦克风还没
    /// 真正开始 capture 第一帧 PCM。为 true 时前端渲染"待命"光效（柔和呼吸、不接真实
    /// 电平），并暗示用户先别急着开口；`level_handler` 首次触发（PCM 真的流入）后翻成
    /// false，光条"点亮"进入正式录音态。只对 Recording 状态有意义。详见胶囊出现时序改造。
    #[serde(default)]
    pub warming: bool,
    /// 用户选择的胶囊样式（siri / classic）。随每次状态事件下发，设置里切换后下一次
    /// 录音即生效，胶囊 webview 无需额外请求。
    #[serde(default)]
    pub capsule_style: CapsuleStyle,
}

/// Today's metrics shown on the Overview tab.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TodayMetrics {
    pub chars_today: u64,
    pub segments_today: u64,
    pub avg_latency_ms: u64,
    pub total_duration_ms: u64,
}

#[cfg(test)]
mod split_front_app_label_tests {
    use super::{split_front_app_label, split_front_app_opt, FrontApp};

    #[test]
    fn macos_label_splits_into_name_and_bundle() {
        let split = split_front_app_label("Claude (com.anthropic.claudefordesktop)", true);
        assert_eq!(split.name.as_deref(), Some("Claude"));
        assert_eq!(
            split.bundle_id.as_deref(),
            Some("com.anthropic.claudefordesktop")
        );
    }

    #[test]
    fn app_names_containing_spaces_and_parens_still_split_on_the_last_group() {
        let split = split_front_app_label("Visual Studio Code (com.microsoft.VSCode)", true);
        assert_eq!(split.name.as_deref(), Some("Visual Studio Code"));
        assert_eq!(split.bundle_id.as_deref(), Some("com.microsoft.VSCode"));
    }

    /// Windows 拿的是窗口标题，里面的括号是正文的一部分，不是 bundle id。
    /// 平台开关关闭时整串保留——即使括号内容恰好形如反向域名、文件路径或版本号，
    /// 也绝不拆。误拆会把标题截断，显示成半句话，还写入错误的 bundle id。
    #[test]
    fn window_titles_are_never_split_outside_macos() {
        for title in [
            "未命名文档 (未保存)",
            "report.txt (~/Documents)",
            "Inbox (12)",
            "script.py (C:\\dir\\script.py)",
            "会议 (meet.example.com)",
            "卸载 (2.4.1)",
        ] {
            let split = split_front_app_label(title, false);
            assert_eq!(
                split.name.as_deref(),
                Some(title),
                "{title} should stay intact"
            );
            assert_eq!(split.bundle_id, None, "{title} has no bundle id");
        }
    }

    #[test]
    fn bare_names_pass_through() {
        let split = split_front_app_label("Terminal", true);
        assert_eq!(split.name.as_deref(), Some("Terminal"));
        assert_eq!(split.bundle_id, None);
    }

    #[test]
    fn blank_input_yields_nothing() {
        assert_eq!(
            split_front_app_label("", true),
            FrontApp {
                name: None,
                bundle_id: None
            }
        );
        assert_eq!(
            split_front_app_label("   ", true),
            FrontApp {
                name: None,
                bundle_id: None
            }
        );
        assert_eq!(
            split_front_app_label("", false),
            FrontApp {
                name: None,
                bundle_id: None
            }
        );
        assert_eq!(
            split_front_app_label("   ", false),
            FrontApp {
                name: None,
                bundle_id: None
            }
        );
        assert_eq!(
            split_front_app_opt(None),
            FrontApp {
                name: None,
                bundle_id: None
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_restore_clipboard_preference_is_ignored_for_true_and_false() {
        for restore in [true, false] {
            let json = serde_json::json!({
                "restoreClipboardAfterPaste": restore,
                "microphoneDeviceName": "USB Mic",
                "streamingInsertSaveClipboard": false,
                "pasteShortcut": "shiftInsert"
            })
            .to_string();
            let preferences: UserPreferences = serde_json::from_str(&json).unwrap();
            assert_eq!(preferences.microphone_device_name, "USB Mic");
            assert!(!preferences.streaming_insert_save_clipboard);
            assert_eq!(preferences.paste_shortcut, PasteShortcut::ShiftInsert);
            let serialized = serde_json::to_value(&preferences).unwrap();
            assert!(serialized.get("restoreClipboardAfterPaste").is_none());
            let loaded: UserPreferences = serde_json::from_value(serialized).unwrap();
            assert_eq!(loaded.microphone_device_name, "USB Mic");
            assert!(!loaded.streaming_insert_save_clipboard);
        }
        assert!(serde_json::to_value(UserPreferences::default())
            .unwrap()
            .get("restoreClipboardAfterPaste")
            .is_none());
    }

    #[test]
    fn macos_newline_modes_round_trip_legacy_auto_and_line_feed() {
        assert_eq!(MacosNewlineMode::default(), MacosNewlineMode::Auto);
        for (wire, expected) in [
            ("\"auto\"", MacosNewlineMode::Auto),
            ("\"shiftReturn\"", MacosNewlineMode::ShiftReturn),
            ("\"lineFeed\"", MacosNewlineMode::LineFeed),
            ("\"return\"", MacosNewlineMode::Return),
        ] {
            let decoded: MacosNewlineMode = serde_json::from_str(wire).unwrap();
            assert_eq!(decoded, expected);
            assert_eq!(serde_json::to_string(&decoded).unwrap(), wire);
        }
    }

    #[test]
    fn removed_pipeline_and_whisper_fields_are_ignored_as_legacy_unknowns() {
        let preferences: UserPreferences = serde_json::from_str(
            r#"{
                "localAsrActiveModel":"qwen3-asr-1.7b",
                "localWhisperActiveModel":"whisper-medium",
                "pipelineMode":"multimodal",
                "multimodalPipelineEnabled":true
            }"#,
        )
        .unwrap();
        assert_eq!(preferences.local_asr_active_model, "qwen3-asr-1.7b");

        let serialized = serde_json::to_value(preferences).unwrap();
        assert!(serialized.get("localWhisperActiveModel").is_none());
        assert!(serialized.get("pipelineMode").is_none());
        assert!(serialized.get("multimodalPipelineEnabled").is_none());
    }

    #[test]
    fn salvage_preserves_valid_fields_when_one_value_is_invalid() {
        // 模拟「某次重构改了字段类型」后的旧文件：historyRetentionDays 是非法值，
        // 但 dictationHotkey / activeAsrProvider 仍然合法。抢救必须保住合法字段，
        // 只把非法字段回落默认——而不是整份丢光。
        let json = br#"{
            "historyRetentionDays": "not-a-number",
            "dictationHotkey": { "primary": "LeftOption", "modifiers": [] },
            "activeAsrProvider": "bailian-qwen3-realtime"
        }"#;

        // 严格解析必失败（否则这个测试没意义）。
        assert!(serde_json::from_slice::<UserPreferences>(json).is_err());

        let salvaged = UserPreferences::salvage_from_json_bytes(json);
        assert_eq!(salvaged.dictation_hotkey.primary, "LeftOption");
        assert_eq!(salvaged.active_asr_provider, "bailian-qwen3-realtime");
        // 非法字段回落到默认，而不是让整份解析失败。
        assert_eq!(
            salvaged.history_retention_days,
            UserPreferences::default().history_retention_days
        );
    }

    #[test]
    fn legacy_style_fields_in_preferences_json_are_ignored_on_load() {
        // 风格系统已删除；旧 preferences.json 里残留的风格字段必须被静默跳过
        // （serde 对未知字段默认忽略），不影响其余设置的加载。
        let json = r#"{
            "defaultMode": "structured",
            "enabledModes": ["raw", "light", "structured", "formal"],
            "activeStylePackId": "custom.meeting",
            "styleSystemPrompts": {"raw": "r", "light": "l", "structured": "s", "formal": "f"},
            "customStylePrompts": {"raw": "", "light": "更像微信消息", "structured": "", "formal": ""},
            "selectionPolishStylePackId": "builtin.light",
            "switchStyleHotkey": {"primary": "S", "modifiers": ["cmd", "shift"]},
            "stylePackHotkeys": [{"packId": "imported.demo", "binding": {"primary": "1", "modifiers": ["alt"]}}],
            "microphoneDeviceName": "External Mic"
        }"#;

        let prefs: UserPreferences = serde_json::from_str(json).unwrap();
        assert_eq!(prefs.microphone_device_name, "External Mic");
        // 序列化结果不再包含任何风格字段。
        let saved = serde_json::to_string(&prefs).unwrap();
        for legacy_key in [
            "defaultMode",
            "enabledModes",
            "activeStylePackId",
            "styleSystemPrompts",
            "customStylePrompts",
            "switchStyleHotkey",
            "stylePackHotkeys",
        ] {
            assert!(
                !saved.contains(legacy_key),
                "serialized preferences must not contain {legacy_key}"
            );
        }
    }

    #[test]
    fn legacy_android_preferences_are_ignored_without_losing_surviving_fields() {
        let json = r#"{
            "androidInsertStrategy": "accessibility",
            "androidOverlayTrigger": "always",
            "androidOverlayActivationMode": "long_press",
            "androidOverlayCancelSwipeDirection": "down",
            "androidOverlaySizeDp": 96,
            "microphoneDeviceName": "External Mic",
            "historyRetentionDays": 30
        }"#;

        let prefs: UserPreferences = serde_json::from_str(json).unwrap();
        assert_eq!(prefs.microphone_device_name, "External Mic");
        assert_eq!(prefs.history_retention_days, 30);

        let saved = serde_json::to_string(&prefs).unwrap();
        for legacy_key in [
            "androidInsertStrategy",
            "androidOverlayTrigger",
            "androidOverlayActivationMode",
            "androidOverlayCancelSwipeDirection",
            "androidOverlaySizeDp",
        ] {
            assert!(!saved.contains(legacy_key));
        }
    }

    #[test]
    fn removed_windows_preferences_are_ignored_as_unknown_fields() {
        let preferences: UserPreferences = serde_json::from_str(
            r#"{
                "windowsInsertionMode":"sendInput",
                "windowsSendInputInsertionOnly":true,
                "windowsSendinputInsertionOnly":true,
                "windowsSendInputNewlineMode":"shiftEnter",
                "windowsSendinputNewlineMode":"shiftEnter",
                "windowsShowOpenlessInKeyboardList":false,
                "allowNonTsfInsertionFallback":false,
                "activeAsrProvider":"preserved-provider"
            }"#,
        )
        .unwrap();
        assert_eq!(preferences.active_asr_provider, "preserved-provider");

        let serialized = serde_json::to_value(preferences).unwrap();
        for removed_key in [
            "windowsInsertionMode",
            "windowsSendInputInsertionOnly",
            "windowsSendinputInsertionOnly",
            "windowsSendInputNewlineMode",
            "windowsSendinputNewlineMode",
            "windowsShowOpenlessInKeyboardList",
            "allowNonTsfInsertionFallback",
        ] {
            assert!(serialized.get(removed_key).is_none(), "{removed_key}");
        }
    }

    #[test]
    fn missing_audio_cue_on_record_pref_defaults_to_enabled() {
        // 老用户的 preferences.json 没有这个字段 → 应默认开启（按下录音即提示）。
        let prefs: UserPreferences = serde_json::from_str("{}").unwrap();

        assert!(prefs.audio_cue_on_record);
    }

    #[test]
    fn capsule_style_pref_defaults_to_siri_and_round_trips_wire_key() {
        // 老用户的 preferences.json 没有 capsuleStyle 字段 → 回落默认 Siri。
        let prefs: UserPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.capsule_style, CapsuleStyle::Siri);

        // 设置里切到 Classic 后：set_settings 存盘（camelCase wire 键）→ 重启
        // get_settings 读回，必须保持 Classic（配置文件持久化 roundtrip）。
        let classic = UserPreferences {
            capsule_style: CapsuleStyle::Classic,
            ..Default::default()
        };
        let json = serde_json::to_string(&classic).unwrap();
        assert!(json.contains(r#""capsuleStyle":"classic""#));
        let restored: UserPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.capsule_style, CapsuleStyle::Classic);
    }

    #[test]
    fn audio_cue_on_record_pref_round_trips_explicit_false() {
        // 用户在设置里关掉后，set_settings → 存盘 → get_settings 必须保住 false，
        // 否则开关一刷新又跳回 true（字段在 Wire 往返时被丢掉的经典症状）。
        let disabled = UserPreferences {
            audio_cue_on_record: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&disabled).unwrap();
        assert!(
            json.contains("\"audioCueOnRecord\":false"),
            "序列化应输出 camelCase 字段，实际: {json}"
        );

        let restored: UserPreferences = serde_json::from_str(&json).unwrap();
        assert!(!restored.audio_cue_on_record);
    }

    #[test]
    fn open_app_hotkey_defaults_to_enabled() {
        // issue #576：默认仍开启（Some 默认键），对老用户零行为变化。
        let prefs = UserPreferences::default();
        assert!(prefs.open_app_hotkey.is_some());
    }

    #[test]
    fn missing_open_app_hotkey_defaults_to_enabled() {
        // 老用户/缺字段：wire 的 struct-default 落到 Some(默认键)，不应被当成停用。
        let prefs: UserPreferences = serde_json::from_str("{}").unwrap();
        assert!(prefs.open_app_hotkey.is_some());
    }

    #[test]
    fn disabled_open_app_hotkey_round_trips_as_null() {
        // issue #576：用户清空（None=停用）后存盘→读回必须仍是 None，
        // 不能像旧逻辑那样被 unwrap_or_else 塌缩回默认键。
        let disabled = UserPreferences {
            open_app_hotkey: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&disabled).unwrap();
        assert!(
            json.contains("\"openAppHotkey\":null"),
            "停用应序列化成 null，实际: {json}"
        );
        let restored: UserPreferences = serde_json::from_str(&json).unwrap();
        assert!(restored.open_app_hotkey.is_none());
    }

    #[test]
    fn explicit_open_app_hotkey_binding_round_trips() {
        // 旧 preferences.json 里带实际绑定 → 读回应保留为 Some（启用）。
        let prefs: UserPreferences = serde_json::from_str(
            r#"{"openAppHotkey":{"primary":"O","modifiers":["cmd","shift"]}}"#,
        )
        .unwrap();
        let binding = prefs.open_app_hotkey.expect("应保留为 Some");
        assert_eq!(binding.primary, "O");
        assert_eq!(
            binding.modifiers,
            vec!["cmd".to_string(), "shift".to_string()]
        );
    }

    /// issue #360: 默认值必须是 CtrlV，跟历史行为一致；老配置文件没有
    /// pasteShortcut 字段时反序列化也得回到 CtrlV，否则会把现有用户的粘贴
    /// 行为静默改掉。
    #[test]
    fn paste_shortcut_defaults_to_ctrl_v() {
        let prefs = UserPreferences::default();
        assert_eq!(prefs.paste_shortcut, PasteShortcut::CtrlV);

        let from_empty: UserPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty.paste_shortcut, PasteShortcut::CtrlV);
    }

    /// issue #440: 老版本会把默认 `streamingInsert:false` 写进 preferences.json。
    /// 缺少迁移标记的旧文件统一迁到 true；带有迁移标记后，用户再手动关掉的 false
    /// 必须保留。
    #[test]
    fn streaming_insert_defaults_to_enabled_for_missing_or_legacy_unmigrated_pref() {
        let prefs = UserPreferences::default();
        assert!(prefs.streaming_insert);
        assert!(prefs.streaming_insert_default_migrated);
        assert!(prefs.streaming_insert_save_clipboard);

        let from_empty: UserPreferences = serde_json::from_str("{}").unwrap();
        assert!(from_empty.streaming_insert);
        assert!(from_empty.streaming_insert_default_migrated);
        assert!(from_empty.streaming_insert_save_clipboard);

        let from_legacy_false: UserPreferences = serde_json::from_str(
            r#"{
                "streamingInsert": false,
                "streamingInsertSaveClipboard": true
            }"#,
        )
        .unwrap();
        assert!(from_legacy_false.streaming_insert);
        assert!(from_legacy_false.streaming_insert_default_migrated);
    }

    #[test]
    fn streaming_insert_preserves_explicit_disabled_value() {
        let prefs: UserPreferences = serde_json::from_str(
            r#"{
                "streamingInsert": false,
                "streamingInsertDefaultMigrated": true,
                "streamingInsertSaveClipboard": false
            }"#,
        )
        .unwrap();

        assert!(!prefs.streaming_insert);
        assert!(prefs.streaming_insert_default_migrated);
        assert!(!prefs.streaming_insert_save_clipboard);
    }

    #[test]
    fn paste_shortcut_round_trips_explicit_values() {
        for (raw, expected) in [
            ("ctrlV", PasteShortcut::CtrlV),
            ("ctrlShiftV", PasteShortcut::CtrlShiftV),
            ("shiftInsert", PasteShortcut::ShiftInsert),
        ] {
            let json = format!(r#"{{ "pasteShortcut": "{raw}" }}"#);
            let prefs: UserPreferences = serde_json::from_str(&json).unwrap();
            assert_eq!(prefs.paste_shortcut, expected, "raw={raw}");
        }
    }

    #[test]
    fn legacy_custom_hotkey_without_custom_binding_is_rejected() {
        let result = serde_json::from_str::<UserPreferences>(
            r#"{
                "hotkey": { "trigger": "custom", "mode": "toggle" }
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn salvage_preserves_valid_fields_when_legacy_custom_hotkey_is_incomplete() {
        let json = br#"{
            "hotkey": { "trigger": "custom", "mode": "toggle", "keys": null },
            "activeAsrProvider": "preserved-provider"
        }"#;

        assert!(serde_json::from_slice::<UserPreferences>(json).is_err());

        let salvaged = UserPreferences::salvage_from_json_bytes(json);
        assert_eq!(salvaged.active_asr_provider, "preserved-provider");
        assert_eq!(salvaged.hotkey, UserPreferences::default().hotkey);
    }

    #[test]
    fn legacy_custom_hotkey_uses_custom_combo_binding() {
        let prefs: UserPreferences = serde_json::from_str(
            r#"{
                "hotkey": { "trigger": "custom", "mode": "toggle" },
                "customComboHotkey": { "primary": "D", "modifiers": ["cmd", "shift"] }
            }"#,
        )
        .unwrap();

        assert_eq!(prefs.dictation_hotkey.primary, "D");
        assert_eq!(prefs.dictation_hotkey.modifiers, vec!["cmd", "shift"]);
    }

    #[test]
    fn custom_hotkey_with_dictation_hotkey_preserves_dictation_binding() {
        let prefs: UserPreferences = serde_json::from_str(
            r#"{
                "hotkey": { "trigger": "custom", "mode": "toggle" },
                "dictationHotkey": { "primary": "Space", "modifiers": ["ctrl"] }
            }"#,
        )
        .unwrap();

        assert_eq!(prefs.dictation_hotkey.primary, "Space");
        assert_eq!(prefs.dictation_hotkey.modifiers, vec!["ctrl"]);
    }

    #[test]
    fn legacy_hotkey_trigger_still_produces_effective_key_codes() {
        let binding: HotkeyBinding =
            serde_json::from_str(r#"{"trigger":"rightControl","mode":"toggle"}"#).unwrap();

        assert_eq!(binding.effective_codes(), vec!["ControlRight".to_string()]);
        assert_eq!(binding.display_label(), "右 Control");
    }

    #[test]
    fn hotkey_binding_supports_combo_side_keys_mouse_and_double_click_mode() {
        let binding = HotkeyBinding {
            trigger: HotkeyTrigger::RightControl,
            mode: HotkeyMode::DoubleClick,
            keys: Some(vec![
                HotkeyKey::new("ControlLeft"),
                HotkeyKey::new("AltLeft"),
                HotkeyKey::new("Mouse4"),
            ]),
        };

        assert_eq!(
            binding.effective_codes(),
            vec![
                "ControlLeft".to_string(),
                "AltLeft".to_string(),
                "Mouse4".to_string()
            ]
        );
        assert_eq!(binding.display_label(), "左Ctrl+左Alt+Mouse4");

        let json = serde_json::to_value(&binding).unwrap();
        assert_eq!(json["mode"], "doubleClick");
    }

    #[test]
    fn explicit_empty_hotkey_keys_clear_the_binding() {
        let binding: HotkeyBinding =
            serde_json::from_str(r#"{"trigger":"rightControl","mode":"toggle","keys":[]}"#)
                .unwrap();

        assert!(binding.effective_codes().is_empty());
    }

    /// PR #826：新增的模型/耗时字段必须向后兼容——旧 history.json 完全没有这些 key。
    /// 风格系统删除后，旧记录里的 `mode` / `stylePackId` 残留标签也必须被静默跳过。
    #[test]
    fn dictation_session_deserializes_legacy_json_without_model_fields() {
        let legacy = r#"{
            "id": "abc",
            "createdAt": "2026-07-01T00:00:00Z",
            "rawTranscript": "你好",
            "finalText": "你好。",
            "mode": "light",
            "stylePackId": "builtin.light",
            "appBundleId": null,
            "appName": null,
            "insertStatus": "inserted",
            "errorCode": null,
            "durationMs": 1200,
            "dictionaryEntryCount": null
        }"#;
        let session: DictationSession = serde_json::from_str(legacy).expect("legacy json");
        assert_eq!(session.source, HistorySource::Voice);
        assert_eq!(session.asr_provider, None);
        assert_eq!(session.asr_model, None);
        assert_eq!(session.llm_provider, None);
        assert_eq!(session.llm_model, None);
        assert_eq!(session.asr_ms, None);
        assert_eq!(session.polish_ms, None);
    }

    /// 新字段序列化必须是 camelCase（前端 types.ts 镜像按 camelCase 读）。
    #[test]
    fn dictation_session_serializes_model_fields_as_camel_case() {
        let session = DictationSession {
            id: "abc".into(),
            created_at: "2026-07-01T00:00:00Z".into(),
            source: HistorySource::SelectionPolish,
            raw_transcript: "你好".into(),
            asr_transcript: None,
            final_text: "你好。".into(),
            polish_source: None,
            app_bundle_id: None,
            app_name: None,
            insert_status: InsertStatus::Inserted,
            error_code: None,
            duration_ms: Some(1200),
            dictionary_entry_count: None,
            has_audio_recording: None,
            asr_provider: Some("bailian".into()),
            asr_model: Some("fun-asr-realtime".into()),
            llm_provider: Some("ark".into()),
            llm_model: Some("deepseek-v3-2".into()),
            asr_ms: Some(230),
            polish_ms: Some(1450),
            pipeline_mode: None,
            polish_diagnostics: Some(crate::polish_diagnostics::PolishDiagnostics {
                stage: crate::polish_diagnostics::PolishDiagnosticsStage::Succeeded,
                outcome: crate::polish_diagnostics::PolishDiagnosticsOutcome::Succeeded,
                provider: Some("local-llama".into()),
                model: Some("gemma-4-E2B-it-Q4_0.gguf".into()),
                template: Some(crate::polish_diagnostics::PolishTemplateKind::Gemma4Canonical),
                add_bos: Some(true),
                add_bos_note: Some(crate::polish_diagnostics::ADD_BOS_NOTE.into()),
                prompt: Some("<|turn>model\n".into()),
                messages: Some(vec![crate::polish_diagnostics::PolishDiagnosticMessage {
                    role: "user".into(),
                    content: "hi".into(),
                }]),
                prompt_tokens: Some(9),
                context_tokens: Some(16_384),
                generation_budget: Some(1_024),
                error: None,
                not_called_reason: None,
                cursor: Some(crate::polish_diagnostics::PolishContextDiagnostics {
                    status: crate::polish_diagnostics::PolishContextStatus::Included,
                    reason: None,
                    text: Some("before".into()),
                }),
                screen_ocr: None,
            }),
        };
        let json = serde_json::to_value(&session).expect("serialize");
        assert_eq!(json["source"], "selection_polish");
        assert_eq!(json["asrProvider"], "bailian");
        assert_eq!(json["asrModel"], "fun-asr-realtime");
        assert_eq!(json["llmProvider"], "ark");
        assert_eq!(json["llmModel"], "deepseek-v3-2");
        assert_eq!(json["asrMs"], 230);
        assert_eq!(json["polishMs"], 1450);
        // 诊断字段按 camelCase 序列化，前端 types.ts 镜像直接可读。
        assert_eq!(json["polishDiagnostics"]["stage"], "succeeded");
        assert_eq!(
            json["polishDiagnostics"]["model"],
            "gemma-4-E2B-it-Q4_0.gguf"
        );
        assert_eq!(json["polishDiagnostics"]["template"], "gemma4Canonical");
        assert_eq!(json["polishDiagnostics"]["promptTokens"], 9);
        assert_eq!(json["polishDiagnostics"]["messages"][0]["role"], "user");
        assert_eq!(json["polishDiagnostics"]["cursor"]["status"], "included");
        // 旧 JSON（无 polishDiagnostics 字段）反序列化为 None。
        // 旧 JSON 里残留的风格（mode）与翻译（translationActive）字段静默跳过。
        let legacy: DictationSession = serde_json::from_value(serde_json::json!({
            "id": "old",
            "createdAt": "2026-07-01T00:00:00Z",
            "rawTranscript": "旧记录",
            "finalText": "旧记录",
            "mode": "light",
            "translationActive": true,
            "insertStatus": "inserted"
        }))
        .expect("legacy rows must deserialize");
        assert!(legacy.polish_diagnostics.is_none());
    }

    /// 风格系统 / 翻译功能 / 风格市场已删除：旧 preferences.json 里残留的
    /// 风格、翻译与市场字段必须被 serde 静默跳过，其余设置完整加载。
    #[test]
    fn legacy_preferences_with_style_translation_and_marketplace_fields_still_load() {
        let legacy = serde_json::json!({
            "hotkey": { "trigger": "rightCommand", "mode": "toggle", "keys": null },
            "microphoneDeviceName": "USB Mic",
            // 风格系统残留
            "polishMode": "light",
            "stylePackId": "builtin.light",
            "stylePacks": [{ "id": "custom", "name": "自定义" }],
            "activeStylePackId": "builtin.light",
            // 翻译功能残留
            "translationTargetLanguage": "English",
            "translationHotkey": { "primary": "Shift", "modifiers": [] },
            "chineseScriptPreference": "simplified",
            "outputLanguagePreference": "zhCn",
            // 风格市场残留
            "marketplaceBaseUrl": "https://api.example.com",
            "marketplaceDevLogin": "octocat"
        });

        let preferences: UserPreferences =
            serde_json::from_value(legacy).expect("legacy preferences must deserialize");

        assert_eq!(preferences.microphone_device_name, "USB Mic");
        assert_eq!(preferences.hotkey.trigger, HotkeyTrigger::RightCommand);
        // 序列化结果不得再携带已删除字段。
        let serialized = serde_json::to_value(&preferences).expect("serialize");
        for key in [
            "translationTargetLanguage",
            "translationHotkey",
            "chineseScriptPreference",
            "outputLanguagePreference",
            "marketplaceBaseUrl",
            "marketplaceDevLogin",
            "polishMode",
            "stylePackId",
        ] {
            assert!(
                serialized.get(key).is_none(),
                "deleted field {key} must not serialize"
            );
        }
    }
}
