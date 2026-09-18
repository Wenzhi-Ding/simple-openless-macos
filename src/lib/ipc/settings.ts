import type { UserPreferences } from "../types"
import {
    invokeOrMock,
    requireBackendReady,
    type StartupSnapshot,
} from "./shared"
import { mockSettings, mockSetSettings } from "./mock-data"

export type { StartupSnapshot }

export async function getStartupSnapshot(): Promise<StartupSnapshot> {
    return requireBackendReady()
}

export function getSettings(): Promise<UserPreferences> {
    return invokeOrMock("get_settings", undefined, () => ({ ...mockSettings }))
}

export function setSettings(prefs: UserPreferences): Promise<void> {
    return invokeOrMock("set_settings", { prefs }, () => {
        mockSetSettings(prefs)
        return undefined
    })
}
