// modelDownloadState.ts — 下载对话框的纯状态推进（无 React / 无 IPC 依赖），
// 便于单测：进度事件聚合、阶段映射、字节格式化。展示组件只消费这里的纯函数。

import type { ModelDownloadProgress } from './ipc/modelDownload';

export type DownloadRowPhase = 'idle' | 'downloading' | 'finished' | 'cancelled' | 'failed';

export interface DownloadRowState {
  phase: DownloadRowPhase;
  file: string;
  fileIndex: number;
  fileCount: number;
  bytes: number;
  total: number;
  error: string | null;
}

export function idleDownloadRow(): DownloadRowState {
  return {
    phase: 'idle',
    file: '',
    fileIndex: 0,
    fileCount: 0,
    bytes: 0,
    total: 0,
    error: null,
  };
}

/** 推进一行状态。终态（finished/cancelled/failed）之后忽略迟到的 progress，
 *  避免轮询重放或事件乱序把完成态拉回下载中。 */
export function applyDownloadProgress(
  state: DownloadRowState,
  progress: ModelDownloadProgress,
): DownloadRowState {
  if (state.phase === 'finished' && progress.phase === 'progress') return state;
  switch (progress.phase) {
    case 'started':
      return {
        ...state,
        phase: 'downloading',
        file: '',
        bytes: 0,
        total: progress.bytesTotal || state.total,
        fileCount: progress.fileCount || state.fileCount,
        error: null,
      };
    case 'progress':
      return {
        ...state,
        phase: 'downloading',
        file: progress.file,
        fileIndex: progress.fileIndex,
        fileCount: progress.fileCount || state.fileCount,
        bytes: progress.bytesDownloaded,
        total: progress.bytesTotal || state.total,
        error: null,
      };
    case 'finished':
      return {
        ...state,
        phase: 'finished',
        file: '',
        bytes: progress.bytesDownloaded || state.total,
        total: progress.bytesTotal || state.total,
        error: null,
      };
    case 'cancelled':
      return { ...state, phase: 'cancelled', file: '', error: null };
    case 'failed':
      return {
        ...state,
        phase: 'failed',
        file: '',
        error: progress.error ?? 'download failed',
      };
    default:
      return state;
  }
}

/** 字节数 → 展示文本：≥1GB 一位小数 GB；≥1MB 一位小数 MB；其余 KB。 */
export function formatDownloadBytes(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/** 进度百分比；total 未知或非下载态时返回 null（UI 隐藏百分比）。 */
export function downloadRowPercent(state: DownloadRowState): number | null {
  if (state.phase === 'idle' || state.total <= 0) return null;
  return Math.min(100, Math.round((state.bytes / state.total) * 100));
}
