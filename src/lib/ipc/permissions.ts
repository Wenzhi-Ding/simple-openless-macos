import type { PermissionStatus } from "../types"
import { invokeOrMock } from "./shared"

export function checkAccessibilityPermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "check_accessibility_permission",
        undefined,
        () => "granted" as const,
    )
}

export function requestAccessibilityPermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "request_accessibility_permission",
        undefined,
        () => "granted" as const,
    )
}

export function checkMicrophonePermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "check_microphone_permission",
        undefined,
        () => "granted" as const,
    )
}

export function requestMicrophonePermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "request_microphone_permission",
        undefined,
        () => "granted" as const,
    )
}

export function openSystemSettings(
    pane: "accessibility" | "microphone" | "screenCapture",
): Promise<void> {
    return invokeOrMock("open_system_settings", { pane }, () => undefined)
}

export function checkScreenCapturePermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "check_screen_capture_permission",
        undefined,
        () => "granted" as const,
    )
}

export function requestScreenCapturePermission(): Promise<PermissionStatus> {
    return invokeOrMock(
        "request_screen_capture_permission",
        undefined,
        () => "granted" as const,
    )
}
