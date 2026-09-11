import type {
    ActivityDay,
    DictationSession,
    DictionaryEntry,
    HotkeyCapability,
    HotkeyStatus,
    PolishDiagnostics,
    UserPreferences,
    MicrophoneDevice,
} from "../types"
import { OL_DATA } from "../mockData"
import {
    defaultAppShortcutModifiers,
} from "../hotkey"

export let mockSettings: UserPreferences = {
    hotkey: {
        trigger: "rightCommand",
        mode: "toggle",
        keys: [{ code: "MetaRight" }],
    },
    dictationHotkey: { primary: "RightCommand", modifiers: [] },
    launchAtLogin: false,
    showCapsule: true,
    capsuleStyle: 'siri',
    muteDuringRecording: false,
    audioCueOnRecord: true,
    silenceAutoStopEnabled: false,
    silenceAutoStopSeconds: 3,
    microphoneDeviceName: "",
    activeAsrProvider: "local-qwen3-mlx",
    activeLlmProvider: "local-llama",
    llmThinkingEnabled: false,
    useSystemProxy: false,
    pasteShortcut: "ctrlV",
    macosNewlineMode: "auto",
    workingLanguages: ["简体中文"],
    customComboHotkey: null,
    openAppHotkey: { primary: "O", modifiers: defaultAppShortcutModifiers() },
    localAsrActiveModel: "qwen3-asr-1.7b",
    localAsrKeepLoadedSecs: 300,
    historyRetentionDays: 7,
    polishContextWindowMinutes: 5,
    startMinimized: false,
    themeMode: "system",
    streamingInsert: true,
    streamingInsertDefaultMigrated: true,
    streamingInsertSaveClipboard: true,
    cursorContextEnabled: true,
    screenOcrContextEnabled: true,
    showOverviewActivityHeatmap: true,
    stackedRowLayout: false,
    conservativeLayout: false,
    autoUpdateCheck: false,
    historyMaxEntries: null,
    recordAudioForDebug: false,
    audioRecordingMaxEntries: null,
}

export const mockHotkeyCapability: HotkeyCapability = {
    adapter: "macEventTap",
    availableTriggers: ["rightCommand", "leftCommand", "rightOption", "leftOption", "custom"],
    requiresAccessibilityPermission: true,
    supportsModifierOnlyTrigger: true,
    supportsSideSpecificModifiers: true,
    explicitFallbackAvailable: false,
    statusHint: "アクセシビリティ権限を許可して右 Command で録音します。",
}

export const mockHotkeyStatus: HotkeyStatus = {
    adapter: "macEventTap",
    state: "installed",
    message: "macOS event tap is installed",
    lastError: null,
}

export const mockMicrophoneDevices: MicrophoneDevice[] = [
    { name: "Built-in Microphone", isDefault: true },
    { name: "USB Microphone", isDefault: false },
]

const mockPolishDiagnostics: (PolishDiagnostics | null)[] = [
    {
        stage: "succeeded",
        outcome: "succeeded",
        provider: "local-llama",
        model: "gemma-4-E2B-it-Q4_0.gguf",
        template: "gemma4Canonical",
        addBos: true,
        addBosNote: "BOS token is injected by the llama.cpp tokenizer via AddBos::Always; the rendered prompt text itself never contains <bos>.",
        prompt: "<|turn>system\n# 角色\n你是整洁的中文文字助手…\n<screen_ocr>\n设置\n声音与屏幕\n</screen_ocr>\n<|turn>user\n<raw_transcript>\n帮我把这句话整理一下明天上午十点和产品团队开会\n</raw_transcript>\n<|turn>model\n",
        messages: [
            { role: "system", content: "# 角色\n你是整洁的中文文字助手…" },
            { role: "user", content: "<raw_transcript>帮我把这句话整理一下…</raw_transcript>" },
        ],
        promptTokens: 512,
        contextTokens: 16384,
        generationBudget: 1024,
        error: null,
        notCalledReason: null,
        cursor: { status: "included", reason: null, text: "before <OPENLESS_CURSOR> after" },
        screenOcr: { status: "included", reason: null, text: "设置\n声音与屏幕" },
    },
    {
        stage: "generating",
        outcome: "failed",
        provider: "local-llama",
        model: "gemma-4-E2B-it-Q4_0.gguf",
        template: "gemma4Canonical",
        addBos: true,
        addBosNote: "BOS token is injected by the llama.cpp tokenizer via AddBos::Always; the rendered prompt text itself never contains <bos>.",
        prompt: "<|turn>system\n…\n<|turn>model\n",
        messages: [{ role: "system", content: "…" }],
        promptTokens: 4096,
        contextTokens: 16384,
        generationBudget: 1024,
        error: {
            code: "InvalidArgument",
            message:
                "local LLM output exceeded its 1024-token limit after a 4096-token prompt; generated 4096 characters",
        },
        notCalledReason: null,
        cursor: { status: "notAdopted", reason: null, text: "before <OPENLESS_CURSOR> after" },
        screenOcr: { status: "notReady", reason: "observation not ready at the freeze point", text: null },
    },
    {
        stage: "notCalled",
        outcome: "notCalled",
        provider: "local-llama",
        model: null,
        template: null,
        addBos: null,
        addBosNote: null,
        prompt: null,
        messages: null,
        promptTokens: null,
        contextTokens: null,
        generationBudget: null,
        error: null,
        notCalledReason: "raw passthrough: the polisher was bypassed",
        cursor: { status: "disabled", reason: "cursor context disabled", text: null },
        screenOcr: { status: "disabled", reason: "screen context disabled", text: null },
    },
    null,
];

export const mockHistory: DictationSession[] = OL_DATA.history.map((h, i) => ({
    id: `mock-${i}`,
    createdAt: new Date().toISOString(),
    rawTranscript: h.preview,
    asrTranscript: null,
    finalText: h.preview,
    polishSource: null,
    appBundleId: null,
    appName: "VS Code",
    insertStatus: "inserted",
    errorCode: null,
    durationMs: 600,
    dictionaryEntryCount: 28,
    hasAudioRecording: null,
    // 模拟当前固定本地引擎；i%4==3 保留旧条目缺耗时/模型归因的展示形态。
    asrProvider: i % 4 === 3 ? null : "local-qwen3-mlx",
    asrModel: i % 4 === 3 ? null : "qwen3-asr-1.7b",
    llmProvider: i % 4 === 3 ? null : "local-llama",
    llmModel: i % 4 === 3 ? null : "gemma-4-E4B-it-Q4_0.gguf",
    asrMs: [120, 64, 9840, null][i % 4],
    polishMs: [1240, 890, 12400, null][i % 4],
    // 润色诊断轮换（见上方 mockPolishDiagnostics）：成功 / 预算耗尽失败 /
    // Raw 未调用 / 旧记录无字段。
    polishDiagnostics: mockPolishDiagnostics[i % 4] ?? null,
}))

export const mockVocab: DictionaryEntry[] = OL_DATA.vocab.map((v, i) => ({
    id: `vocab-${i}`,
    phrase: v.word,
    note: null,
    enabled: true,
    hits: v.count,
    createdAt: new Date().toISOString(),
}))

export function mockSetSettings(prefs: UserPreferences): void {
    mockSettings = { ...prefs }
}

// ── 活动热力图（浏览器 dev 演示数据）────────────────────────────────────
// 过去一年稀疏分布的日计数，铺出有疏密对比的热力图。种子取日期序号的伪随机，
// 刷新之间保持稳定。
export const mockActivityDays: ActivityDay[] = (() => {
    const days: ActivityDay[] = []
    const today = new Date()
    for (let i = 364; i >= 0; i -= 1) {
        const d = new Date(today)
        d.setDate(today.getDate() - i)
        const seed = Math.abs(Math.sin(i * 12.9898) * 43758.5453) % 1
        if (seed < 0.55) continue
        const count = Math.max(1, Math.round(seed * 22) - 8)
        const iso = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`
        // 字数 / 时长按每条 ~120 字、~9 秒的量级派生，让周期指标卡在浏览器 dev 下
        // 也有可看的数据。最早的 30 天故意只给 count（不给 chars/durationMs），
        // 模拟升级前写入的老数据，验证「老日期在字数/时长指标里显示 0」不会崩。
        const legacy = i > 334
        days.push(
            legacy
                ? { date: iso, count }
                : {
                      date: iso,
                      count,
                      chars: count * (90 + Math.round(seed * 70)),
                      durationMs: count * (6000 + Math.round(seed * 7000)),
                  },
        )
    }
    return days
})()
