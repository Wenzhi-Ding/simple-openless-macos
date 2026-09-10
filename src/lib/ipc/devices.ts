import type { MicrophoneDevice } from "../types"
import { invokeOrMock } from "./shared"
import { mockMicrophoneDevices } from "./mock-data"

export function listMicrophoneDevices(): Promise<MicrophoneDevice[]> {
    return invokeOrMock(
        "list_microphone_devices",
        undefined,
        () => mockMicrophoneDevices,
    )
}

export function startMicrophoneLevelMonitor(deviceName: string): Promise<void> {
    return invokeOrMock(
        "start_microphone_level_monitor",
        { deviceName },
        () => undefined,
    )
}

export function stopMicrophoneLevelMonitor(): Promise<void> {
    return invokeOrMock(
        "stop_microphone_level_monitor",
        undefined,
        () => undefined,
    )
}
