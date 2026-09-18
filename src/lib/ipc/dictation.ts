import { invokeOrMock } from "./shared"

export function stopDictation(): Promise<void> {
    return invokeOrMock("stop_dictation", undefined, () => undefined)
}

export function cancelDictation(): Promise<void> {
    return invokeOrMock("cancel_dictation", undefined, () => undefined)
}
