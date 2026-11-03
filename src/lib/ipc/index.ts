// Barrel — re-exports every public symbol from the domain modules.

export type { PlatformCapabilities } from "../types"

// shared
export { isTauri, invokeOrMock, getPlatformCapabilities } from "./shared"

// settings
export type { StartupSnapshot } from "./settings"
export {
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
    stopDictation,
    cancelDictation,
    getCurrentDictationPhase,
    playCue,
    stopCue,
} from "./dictation"
export type { CueKind } from "./dictation"

// permissions
export {
    checkAccessibilityPermission,
    requestAccessibilityPermission,
    checkMicrophonePermission,
    requestMicrophonePermission,
    openSystemSettings,
} from "./permissions"

// hotkeys
export {
    getHotkeyStatus,
    getHotkeyCapability,
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
