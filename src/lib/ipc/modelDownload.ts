// modelDownload.ts — 首启模型下载的 IPC 包装。
// 下载源（仓库/revision/SHA-256）钉死在 Rust 侧 manifest（src-tauri/src/model_download.rs）；
// 前端只传递模型键与镜像选择，进度经 model-download:progress 事件上报。

import { invokeOrMock } from "./shared"

export type ModelDownloadKey = "asr-bf16" | "asr-4bit" | "llm-gguf"
export type ModelDownloadMirror = "huggingface" | "hf-mirror"

export type ModelDownloadPhase =
  | "started"
  | "progress"
  | "finished"
  | "cancelled"
  | "failed"

/** 与 Rust 侧 DownloadProgress（camelCase serde）逐字段对应。 */
export interface ModelDownloadProgress {
  key: string
  file: string
  fileIndex: number
  fileCount: number
  bytesDownloaded: number
  bytesTotal: number
  phase: ModelDownloadPhase
  error: string | null
}

/** 与 Rust PROGRESS_EVENT 常量一致。 */
export const MODEL_DOWNLOAD_PROGRESS_EVENT = "model-download:progress"

/** 启动一个模型的下载（用户显式点击后调用）；进度经事件上报。 */
export function startModelDownload(
  key: ModelDownloadKey,
  mirror: ModelDownloadMirror,
): Promise<void> {
  return invokeOrMock("model_download_start", { key, mirror }, () => undefined)
}

/** 取消进行中的下载；`.partial` 保留，重试自动续传。 */
export function cancelModelDownload(key: ModelDownloadKey): Promise<void> {
  return invokeOrMock("model_download_cancel", { key }, () => undefined)
}

/** 订阅下载进度事件；返回取消订阅函数。 */
export async function listenModelDownloadProgress(
  handler: (progress: ModelDownloadProgress) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen<ModelDownloadProgress>(
    MODEL_DOWNLOAD_PROGRESS_EVENT,
    ({ payload }) => handler(payload),
  )
}
