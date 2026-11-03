import { invokeOrMock, isTauri, requireBackendReady } from "./shared"

export function stopDictation(): Promise<void> {
    return invokeOrMock("stop_dictation", undefined, () => undefined)
}

export function cancelDictation(): Promise<void> {
    return invokeOrMock("cancel_dictation", undefined, () => undefined)
}

interface CurrentDictationSnapshot {
    backend?: { dictation?: { phase?: string } }
}

/**
 * 读取后端**当前**听写阶段（camelCase: idle/starting/recording/…）。
 *
 * 提示音监听器挂载时用它在订阅前建立基线：录音进行中重挂载时，首条 capsule:state 就
 * 不会被当成「刚开始录音」而误放提示音。这里不能走 invokeOrMock，因为它把
 * get_startup_snapshot 转成启动时缓存的握手快照。后端已运行时 core.start() 只读取当前
 * dictation 快照、不会重启后端（crates/openless-core/src/api.rs:633-645），所以直接
 * invoke 同一条命令拿到的就是当下状态。
 */
export async function getCurrentDictationPhase(): Promise<string> {
    if (!isTauri) return "idle"
    await requireBackendReady()
    const { invoke } = await import("@tauri-apps/api/core")
    const snapshot = await invoke<CurrentDictationSnapshot>("get_startup_snapshot")
    const phase = snapshot.backend?.dictation?.phase
    return typeof phase === "string" ? phase : "idle"
}
