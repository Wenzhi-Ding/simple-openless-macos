// Barrel — re-exports every public symbol from the domain modules.

export type { PlatformCapabilities } from "../types"

// platform
export { isDesktop } from "./platform-exports"

// shared
export { isTauri, invokeOrMock, getPlatformCapabilities } from "./shared"

// settings
export type { StartupSnapshot } from "./settings"
export {
    BACKEND_CONTRACT_VERSION,
    getStartupSnapshot,
    getSettings,
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
    repolish,
} from "./history"

// vocab
export {
    listVocab,
    addVocab,
    removeVocab,
    setVocabEnabled,
    copyTextToClipboard,
    dismissInsertFallbackCard,
    reportInsertFallbackCardHeight,
} from "./vocab"

// dictation
export {
    startDictation,
    stopDictation,
    cancelDictation,
} from "./dictation"

// permissions
export {
    checkAccessibilityPermission,
    requestAccessibilityPermission,
    checkMicrophonePermission,
    requestMicrophonePermission,
    openSystemSettings,
    triggerMicrophonePrompt,
} from "./permissions"

// hotkeys
export {
    getHotkeyStatus,
    getHotkeyCapability,
    validateComboHotkey,
    setComboHotkey,
    validateShortcutBinding,
    setDictationHotkey,
    setShortcutRecordingActive,
} from "./hotkeys"

// devices
export {
    listMicrophoneDevices,
    startMicrophoneLevelMonitor,
    stopMicrophoneLevelMonitor,
} from "./devices"


// utils
export { exportErrorLog, debugReadCursorContext } from "./utils"
