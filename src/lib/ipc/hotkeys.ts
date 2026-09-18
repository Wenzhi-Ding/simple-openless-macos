import type { HotkeyCapability, HotkeyStatus, ShortcutBinding } from "../types"
import { invokeOrMock } from "./shared"
import {
    mockHotkeyStatus,
    mockHotkeyCapability,
} from "./mock-data"

export function getHotkeyStatus(): Promise<HotkeyStatus> {
    return invokeOrMock("get_hotkey_status", undefined, () => mockHotkeyStatus)
}

export function getHotkeyCapability(): Promise<HotkeyCapability> {
    return invokeOrMock("get_hotkey_capability", undefined, () => mockHotkeyCapability)
}

export function validateShortcutBinding(
    binding: ShortcutBinding,
): Promise<void> {
    return invokeOrMock(
        "validate_shortcut_binding",
        { binding },
        () => undefined,
    )
}

export function setDictationHotkey(binding: ShortcutBinding): Promise<void> {
    return invokeOrMock("set_dictation_hotkey", { binding }, () => undefined)
}

export function setShortcutRecordingActive(active: boolean): Promise<void> {
    return invokeOrMock(
        "set_shortcut_recording_active",
        { active },
        () => undefined,
    )
}
