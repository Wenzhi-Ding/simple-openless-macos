// Barrel — re-exports every public symbol from the domain modules.
// 减法改造(纯本地):云端凭据/渠道、QA、Less Computer、Selection Voice、
// Coding Agent、Marketplace、GitHub OAuth、updater、remote-server、
// check_network 的导出已随对应后端命令移除。

export type { UpdateChannel, PlatformCapabilities } from "../types"

// platform & android
export { isAndroid, isDesktop, isMobile } from "./platform-exports"
export {
    getAndroidOverlayStatus,
    requestAndroidOverlayPermission,
    showAndroidOverlay,
    hideAndroidOverlay,
    getAndroidAccessibilityStatus,
    requestAndroidAccessibilityPermission,
} from "./platform-exports"

// shared
export { isTauri, invokeOrMock, getPlatformCapabilities } from "./shared"

// settings
export type { StartupSnapshot } from "./settings"
export {
    BACKEND_CONTRACT_VERSION,
    getStartupSnapshot,
    getSettings,
    getDefaultStyleSystemPrompts,
    setSettings,
} from "./settings"

// history
export {
    listHistory,
    deleteHistoryEntry,
    clearHistory,
    getActivityStats,
    readAudioRecording,
    retranscribeRecording,
} from "./history"

// vocab
export {
    listVocab,
    addVocab,
    removeVocab,
    setVocabEnabled,
    listCorrectionRules,
    addCorrectionRule,
    acceptPendingCorrection,
    rejectPendingCorrection,
    dismissVocabSuggestions,
    copyTextToClipboard,
    dismissInsertFallbackCard,
    reportInsertFallbackCardHeight,
    removeCorrectionRule,
    setCorrectionRuleEnabled,
    listVocabPresets,
    saveVocabPresets,
} from "./vocab"

// dictation
export {
    startDictation,
    stopDictation,
    cancelDictation,
    handleWindowHotkeyEvent,
} from "./dictation"

// style-packs
export {
    repolish,
    setDefaultPolishMode,
    setStyleEnabled,
    listStylePacks,
    saveStylePack,
    createStylePackFromTemplate,
    previewStylePackRuntime,
    setActiveStylePack,
    setStylePackEnabled,
    resetBuiltinStylePack,
    deleteStylePack,
    importStylePackFromZip,
    exportStylePackToZip,
} from "./style-packs"

// permissions
export {
    checkAccessibilityPermission,
    requestAccessibilityPermission,
    checkMicrophonePermission,
    requestMicrophonePermission,
    openSystemSettings,
    triggerMicrophonePrompt,
    restartApp,
    resetAccessibilityPermissionAndRestartApp,
} from "./permissions"

// hotkeys
export {
    getHotkeyStatus,
    getHotkeyCapability,
    getWindowsImeStatus,
    validateComboHotkey,
    setComboHotkey,
    validateShortcutBinding,
    setDictationHotkey,
    setSelectionPolishHotkey,
    setSwitchStyleHotkey,
    setOpenAppHotkey,
    setStylePackHotkeys,
    setShortcutRecordingActive,
} from "./hotkeys"

// devices
export {
    listMicrophoneDevices,
    startMicrophoneLevelMonitor,
    stopMicrophoneLevelMonitor,
    isWaylandCliMode,
} from "./devices"

export {
    getSelectionPolishPreview,
    confirmSelectionPolishPreview,
    cancelSelectionPolishPreview,
} from './selection-polish-preview'

// utils
export { openExternal, exportErrorLog, logClientError, debugReadCursorContext } from "./utils"
