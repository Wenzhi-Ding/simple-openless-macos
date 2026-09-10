// TypeScript mirror of src-tauri/src/types.rs.
// All keys are camelCase (Rust serializes with #[serde(rename_all = "camelCase")]).

export type InsertStatus = 'inserted' | 'pasteSent' | 'copiedFallback' | 'failed';

/** 概览页年度活动热力图的单日计数（date = 本地日期 YYYY-MM-DD）。 */
export interface ActivityDay {
  date: string;
  count: number;
  /** 当日最终插入文本的总字符数。升级前写入的日期没有这个字段（读作 0）。 */
  chars?: number;
  /** 当日录音总时长（毫秒）。升级前写入的日期没有这个字段（读作 0）。 */
  durationMs?: number;
}

export interface DictationSession {
  id: string;
  createdAt: string; // ISO-8601
  rawTranscript: string;
  /** 旧流水线保存的确定性纠正前 ASR 原文。当前固定流程没有单独保留同音纠正前文本，
   *  因此新记录通常为 null；旧历史可能仍有值。 */
  asrTranscript: string | null;
  finalText: string;
  polishSource: string | null;
  appBundleId: string | null;
  appName: string | null;
  insertStatus: InsertStatus;
  errorCode: string | null;
  durationMs: number | null;
  dictionaryEntryCount: number | null;
  /** 该会话是否在录音时归档了原始 wav（取决于当时 prefs.recordAudioForDebug）。
   *  true 时前端在 History 渲染播放按钮，凭 id 通过 read_audio_recording IPC 拿字节流。 */
  hasAudioRecording: boolean | null;
  /** 本次转写用的 ASR provider id（如 "volcengine" / "local-qwen3"）。旧历史为 null。 */
  asrProvider: string | null;
  /** 本次转写用的 ASR 模型 id。provider 无模型概念时为 null。 */
  asrModel: string | null;
  /** 本次润色用的 LLM provider id。Raw 直通（未调用 LLM）时为 null。 */
  llmProvider: string | null;
  /** 本次润色用的 LLM 模型 id。Raw 直通时为 null。 */
  llmModel: string | null;
  /** 松键后等待转写结果的实测耗时（毫秒）。流式 ASR 是收尾延迟，批式是完整转写耗时。 */
  asrMs: number | null;
  /** LLM 润色/翻译调用的实测耗时（毫秒）。未调用 LLM 时为 null。 */
  polishMs: number | null;
  /** 润色调用诊断快照：完整最终 prompt（含 OCR/光标上下文正文）、结构化消息、
   *  token 数、输出预算、渲染路径与调用阶段。用户已授权随本机 history.json 保存；
   *  旧记录 / 重新转写后的条目为 null。见后端 polish_diagnostics 模块。 */
  polishDiagnostics?: PolishDiagnostics | null;
}

/** 诊断里一条实际渲染进最终 prompt 的结构化消息（含真实 prior turns）。 */
export interface PolishDiagnosticMessage {
  role: string;
  content: string;
}

/** 上下文（光标文档 / 屏幕 OCR）在本次调用中的结局。 */
export type PolishContextStatus =
  | 'included' | 'notAdopted' | 'disabled' | 'notSupported' | 'notStarted'
  | 'permissionDenied' | 'sensitiveBlocked' | 'targetChanged' | 'timeout'
  | 'notReady' | 'empty' | 'failed' | 'blocked' | 'unavailable' | 'unknown';

export interface PolishContextDiagnostics {
  status: PolishContextStatus;
  /** 机器可读原因（不含正文的短串）。 */
  reason: string | null;
  /** 真正进入（或本可进入）prompt 的处理后正文；未捕获为 null。 */
  text: string | null;
}

export type PolishDiagnosticsStage =
  | 'notCalled' | 'modelMissing' | 'modelLoadFailed' | 'renderFailed'
  | 'rendered' | 'tokenized' | 'prefilled' | 'generating' | 'succeeded';

export type PolishDiagnosticsOutcome = 'succeeded' | 'failed' | 'notCalled';

export type PolishTemplateKind = 'gemma4Canonical' | 'ggufChatTemplate';

export interface PolishDiagnosticError {
  code: string;
  /** 脱敏摘要（只含计数/状态词，不含 prompt 正文）。 */
  message: string;
}

export interface PolishDiagnostics {
  stage: PolishDiagnosticsStage;
  outcome: PolishDiagnosticsOutcome;
  provider: string | null;
  model: string | null;
  template: PolishTemplateKind | null;
  addBos: boolean | null;
  addBosNote: string | null;
  prompt: string | null;
  messages: PolishDiagnosticMessage[] | null;
  promptTokens: number | null;
  contextTokens: number | null;
  generationBudget: number | null;
  error: PolishDiagnosticError | null;
  notCalledReason: string | null;
  cursor: PolishContextDiagnostics | null;
  screenOcr: PolishContextDiagnostics | null;
}

export interface DictionaryEntry {
  id: string;
  phrase: string;
  note: string | null;
  enabled: boolean;
  hits: number;
  createdAt: string;
}

/** `debug_read_cursor_context` 的返回：一次光标上下文探测的完整结果。
 *  status 之外的每一种都要能说清「为什么没读到」——装机验证时全靠它判断某个 app
 *  是被安全闸门拦住了，还是 AX 根本不支持。 */
export interface HostDocumentReadResult {
  status: 'ok' | 'blocked' | 'unsupported' | 'unavailable' | 'timeout';
  reason: string | null;
  window: { text: string; cursor: number } | null;
  appName: string | null;
  bundleId: string | null;
  elapsedMs: number;
}

/** 为什么这段话没落进目标 app。只用于后端日志，卡片本身不渲染它。 */
export type InsertFallbackReason = 'partialStream' | 'insertFailed';

/** 落字失败兜底卡片的内容。`text` 始终是完整的那段话，即便屏幕上只落了半截。 */
export interface InsertFallbackCardPayload {
  text: string;
  reason: InsertFallbackReason;
  /** 本次展示代次；尺寸回报必须原样携带，后端据此忽略旧卡片的迟到 IPC。 */
  presentationId: number;
}

export type HotkeyTrigger =
  | 'rightOption'
  | 'leftOption'
  | 'rightControl'
  | 'leftControl'
  | 'rightCommand'
  | 'leftCommand'
  | 'leftShift'
  | 'rightShift'
  | 'fn'
  | 'rightAlt'
  | 'mediaPlayPause'
  | 'custom';

export type HotkeyMode = 'toggle' | 'hold' | 'doubleClick' | 'auto';

export interface HotkeyKey {
  code: string;
}

export interface HotkeyBinding {
  trigger: HotkeyTrigger;
  mode: HotkeyMode;
  keys?: HotkeyKey[] | null;
}

export type HotkeyAdapterKind = 'macEventTap' | 'windowsLowLevel' | 'fcitx5' | 'unavailable';

export interface HotkeyCapability {
  adapter: HotkeyAdapterKind;
  availableTriggers: HotkeyTrigger[];
  requiresAccessibilityPermission: boolean;
  supportsModifierOnlyTrigger: boolean;
  supportsSideSpecificModifiers: boolean;
  explicitFallbackAvailable: boolean;
  statusHint: string | null;
}

export interface HotkeyInstallError {
  code: string;
  message: string;
}

export type HotkeyStatusState = 'starting' | 'installed' | 'failed';

export interface HotkeyStatus {
  adapter: HotkeyAdapterKind;
  state: HotkeyStatusState;
  message: string | null;
  lastError: HotkeyInstallError | null;
}

export interface ShortcutBinding {
  /** 主键，例如 "D" / "Space" / "F1" / "RightOption" / "LeftShift" */
  primary: string;
  /** 修饰符：泛化 tag（cmd/ctrl/…）或侧别 tag（cmd-left/ctrl-right/…）。 */
  modifiers: string[];
}

/** 自定义录音组合键绑定。当 hotkey.trigger == 'custom' 时使用。 */
export type ComboBinding = ShortcutBinding;

/** 模拟粘贴时按下的快捷键。仅 Windows/Linux 生效；macOS 走 AX 直写。
 *  - ctrlV       : 标准粘贴（默认；大多数编辑器、浏览器、IDE）
 *  - ctrlShiftV  : kitty / alacritty / wezterm / gnome-terminal / foot 等终端
 *  - shiftInsert : xterm / urxvt 等老派 X11 终端
 *  详见 issue #360。 */
export type PasteShortcut = 'ctrlV' | 'ctrlShiftV' | 'shiftInsert';

/** Windows 听写文本插入策略。 */
export type WindowsInsertionMode = 'tsf' | 'sendInput' | 'paste';

/** Windows SendInput 路径的换行模拟方式。 */
export type WindowsSendInputNewlineMode = 'enter' | 'shiftEnter' | 'crlf';

/** macOS 逐字上屏时换行符怎么发。`auto` 按前台应用选择；`lineFeed` 供终端使用；
 *  `return` 在聊天框里等于发送 —— 靠换行拆多条消息的风格包要的就是这个。 */
export type MacosNewlineMode = 'auto' | 'shiftReturn' | 'lineFeed' | 'return';

export type ThemeMode = 'system' | 'light' | 'dark';

export interface UserPreferences {
  hotkey: HotkeyBinding;
  dictationHotkey: ShortcutBinding;
  launchAtLogin: boolean;
  showCapsule: boolean;
  /** 录音胶囊样式（'siri' | 'classic'）。见 CapsulePayload.capsuleStyle 的运行时下发。 */
  capsuleStyle: CapsuleStyle;
  /** 录音期间临时静音系统输出，停止/取消/出错后恢复原静音状态。 */
  muteDuringRecording: boolean;
  /** 按下录音热键进入 recording 状态时，播放一段合成提示音提醒「已开始录音」。
   *  默认开启；在 capsule 窗口用 Web Audio API 合成，不依赖 showCapsule。 */
  audioCueOnRecord: boolean;
  /** Toggle 模式「说完自动停止」（issue #860）。默认关闭；开启后检测到语音、
   *  连续静音达到 silenceAutoStopSeconds 时自动停止并提交，一直没说话则 10 秒后取消。 */
  silenceAutoStopEnabled: boolean;
  /** 语音后的连续静音阈值（秒）。可选 1 / 1.5 / 2 / 3 / 4 / 5，默认 3。 */
  silenceAutoStopSeconds: number;
  /** 录音输入设备名称。空字符串 = 使用系统默认麦克风。 */
  microphoneDeviceName: string;
  activeAsrProvider: string;
  activeLlmProvider: string;
  /** LLM 思考模式开关。默认关闭；OpenAI 普通 chat 模型会跳过不支持的字段。详见 issue #402。 */
  llmThinkingEnabled: boolean;
  /** 是否使用系统代理（issue #869）。默认开启；关闭后所有请求直连，境外服务（GitHub 登录/更新等）可能连不上。 */
  useSystemProxy: boolean;
  /** 仅 Windows/Linux：粘贴成功后是否恢复用户原剪贴板。默认 true。详见 issue #111。 */
  restoreClipboardAfterPaste: boolean;
  /** 仅 Windows/Linux：模拟粘贴时按下的快捷键。详见 issue #360：kitty/alacritty
   *  等终端只接受 Ctrl+Shift+V，硬编码 Ctrl+V 会被吞掉，听写文本只剩在剪贴板里。
   *  macOS 走 AX 直写不受影响。默认 'ctrlV' 与历史行为一致。 */
  pasteShortcut: PasteShortcut;
  macosNewlineMode: MacosNewlineMode;
  /** 用户的工作语言（多选，原生名）；作为前提注入 LLM polish prompt 头部。 */
  workingLanguages: string[];
  /** 自定义录音组合键。当 hotkey.trigger == 'custom' 时使用。null = 未设置。 */
  customComboHotkey: ComboBinding | null;
  /** 打开 OpenLess 主窗口的全局快捷键。null = 用户已停用（issue #576）。 */
  openAppHotkey: ShortcutBinding | null;
  /** 本地 Qwen3-ASR 当前激活的模型 id。仅在 local-qwen3 系列 provider 时有意义。 */
  localAsrActiveModel: string;
  /** 本地 ASR 引擎在内存中的保留时长（秒）。0 = 说完话即释放；
   *  300 = 默认 5 分钟；86400 ≈ 不释放（保持加载）。 */
  localAsrKeepLoadedSecs: number;
  /** 历史记录保留天数。0 = 不按时间清理（仍受 200 条上限）。默认 7。 */
  historyRetentionDays: number;
  /** 对话感知 polish 上下文窗口（分钟）。0 = 关闭。默认 5。详见 PR-A。 */
  polishContextWindowMinutes: number;
  /** 启动时静默运行（不弹主窗口）。Windows 开机自启场景常用——只想要后台 + 托盘，
   *  不想被主窗口打扰。开后所有启动路径都不弹窗，从菜单栏 / 托盘进入主窗口。默认 false。 */
  startMinimized: boolean;
  /** UI theme preference: follow OS, light, or dark. */
  themeMode: ThemeMode;
  /** 流式输入：润色 SSE 一边到达一边逐字模拟键盘事件输出到当前焦点。开启后用户感知到
   *  的处理时延显著降低。v1 限定 macOS + OpenAI-compatible provider，其他配置自动回落
   *  到原一次性插入。默认 true。 */
  streamingInsert: boolean;
  /** issue #440 一次性迁移标记：旧配置缺少该字段时后端会把老默认 false 迁到 true；
   *  迁移后用户再手动关掉 streamingInsert 时保留 false。 */
  streamingInsertDefaultMigrated: boolean;
  /** 流式输入成功后是否把最终润色文本写回剪贴板。开启后 Cmd+V 还能重复粘贴该次输出，
   *  与一次性路径行为对齐。默认 true。 */
  streamingInsertSaveClipboard: boolean;
  /** 是否把「用户正在写的那篇文档」中光标附近的原文送进 LLM 润色当上下文。
   *  默认 false —— 开启后每次听写都会读取前台 app 的正文并把其中一段发给 LLM 服务商。
   *  仅 macOS 有实现；密码框 / Secure Input / 密码管理器 / 终端一律硬拦。 */
  cursorContextEnabled: boolean;
  /** 屏幕上下文（实验）：听写开始时对前台窗口做一次本地 Vision OCR，把识别文字
   *  作为不可信背景参考送进润色（仅消歧义/专名）。截图只在内存，不落盘不进日志。
   *  默认 false；需 macOS 屏幕录制 + 辅助功能权限；敏感应用/Secure Input 一律跳过。 */
  screenOcrContextEnabled: boolean;
  /** 概览页是否显示「年度活动」热力图卡。默认 true；关闭只隐藏卡片，活动计数照常记录。 */
  showOverviewActivityHeatmap: boolean;
  /** 易读布局：小屏或大字号时强制同行控件换行，避免横向溢出。默认 false。 */
  stackedRowLayout: boolean;
  /** 保守排版：除首页、顶栏、底栏与胶囊窗外，内容区强制单列满宽。默认 false。 */
  conservativeLayout: boolean;
  /** 主窗口启动 + 后台每 60 分钟自动检查更新。默认 true。
   *  桌面：开启后自动检查，发现更新弹窗由用户确认安装。
   *  关闭后仅 Settings 手动「检查更新」按钮可用。 */
  autoUpdateCheck: boolean;
  /** 历史记录上限（条数）。null = 走默认 200；5..=200 之间为用户自定义。 */
  historyMaxEntries: number | null;
  /** 是否为每次会话保留原始麦克风音频文件（wav），用于排查 ASR 误识别 / 麦克风灵敏度。
   *  默认 false。开启后会占磁盘空间，受 historyRetentionDays 同样的清理策略约束。 */
  recordAudioForDebug: boolean;
  /** recordings/ 里保留的最近 wav 文件数。null = 跟随 200 硬上限；1..=200 之间为用户自定义。
   *  跟 historyMaxEntries 解耦——「文本档案多但 wav 只留最近 5 条」是合法组合。 */
  audioRecordingMaxEntries: number | null;
}

export interface MicrophoneDevice {
  name: string;
  isDefault: boolean;
}

/** 内置语言列表 — 前端 Settings UI 用，后端只接收原生名字符串拼 prompt。
 *  添加新语言时直接在这里加一项（原生名），无需修改后端。 */
export const SUPPORTED_LANGUAGES: readonly string[] = [
  '简体中文',
  '繁体中文',
  'English',
  '日本語',
  '한국어',
  'Français',
  'Deutsch',
  'Español',
  'Italiano',
  'Português',
  'Русский',
  'العربية',
  'Tiếng Việt',
  'ไทย',
  'हिन्दी',
] as const;

export type CapsuleState =
  | 'idle'
  | 'recording'
  | 'transcribing'
  | 'polishing'
  | 'done'
  | 'cancelled'
  | 'error';

/** 录音胶囊样式：'siri' = 流光 Siri 光效版（默认）；'classic' = Openless 经典药丸版。 */
export type CapsuleStyle = 'siri' | 'classic';

export interface CapsulePayload {
  state: CapsuleState;
  level: number; // 0..1 RMS
  elapsedMs: number;
  message: string | null;
  insertedChars: number | null;
  /**
   * 预备态：胶囊已「乐观显示」（按下热键即弹出并播入场动画），但麦克风还没吐第一帧
   * PCM。为 true 时录音光条渲染成「待命」形态（柔和呼吸、不接真实电平），暗示用户稍候
   * 再开口；麦克风就绪后翻 false，光条点亮进入正式录音。只对 recording 有意义。
   */
  warming?: boolean;
  /**
   * 用户选择的胶囊样式（siri / classic）。随每次状态事件下发；缺失时回落默认
   * 'siri'，兼容旧后端 payload。
   */
  capsuleStyle?: CapsuleStyle;
}

export interface TodayMetrics {
  charsToday: number;
  segmentsToday: number;
  avgLatencyMs: number;
  totalDurationMs: number;
}

export type PermissionStatus =
  | 'granted'
  | 'denied'
  | 'notDetermined'
  | 'restricted'
  | 'notApplicable'
  | 'noDevice';

/** Runtime platform kind returned by `get_platform_capabilities`. */
export type PlatformKind = 'macos';

/** Runtime capabilities reported by the desktop host. */
export interface PlatformCapabilities {
  platform: PlatformKind;
  supportsDesktopHotkey: boolean;
  supportsTray: boolean;
  supportsOverlay: boolean;
  supportsImeInput: boolean;
  supportsLocalAsr: boolean;
  supportsLocalQwen3Mlx: boolean;
  supportsInAppDictation: boolean;
  supportsAutoUpdate: boolean;
}
