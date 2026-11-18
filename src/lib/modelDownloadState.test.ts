// modelDownloadState.test.ts — 下载状态推进的最小行为契约：
// 终态不被迟到 progress 拉回、bytes/total 聚合正确、终态携带错误文本、
// 字节格式化与百分比边界。

import {
  applyDownloadProgress,
  downloadRowPercent,
  formatDownloadBytes,
  idleDownloadRow,
} from './modelDownloadState';
import type { ModelDownloadProgress } from './ipc/modelDownload';

function assert(condition: boolean, message = 'condition must hold') {
  if (!condition) throw new Error(message);
}
function equal<T>(actual: T, expected: T, message = '') {
  if (actual !== expected) {
    throw new Error(`${message}: actual=${String(actual)} expected=${String(expected)}`);
  }
}

function progressEvent(
  phase: ModelDownloadProgress['phase'],
  overrides: Partial<ModelDownloadProgress> = {},
): ModelDownloadProgress {
  return {
    key: 'asr-bf16',
    file: 'model.safetensors',
    fileIndex: 0,
    fileCount: 9,
    bytesDownloaded: 0,
    bytesTotal: 1000,
    phase,
    error: null,
    ...overrides,
  };
}

// started → downloading，记录 total。
let row = applyDownloadProgress(idleDownloadRow(), progressEvent('started', { bytesTotal: 4096 }));
equal(row.phase, 'downloading', 'started 进入下载中');
equal(row.total, 4096, 'started 记录总字节');

// progress 聚合 bytes；事件缺省 total（0）时沿用上一轮，带 total 时覆盖。
row = applyDownloadProgress(row, progressEvent('progress', { bytesDownloaded: 1024, bytesTotal: 0 }));
equal(row.bytes, 1024, 'progress 累计字节');
equal(row.total, 4096, 'progress 在 total=0 时沿用上一轮');
row = applyDownloadProgress(row, progressEvent('progress', { bytesDownloaded: 2048, bytesTotal: 4096 }));
equal(row.bytes, 2048, 'progress 更新字节');
equal(row.total, 4096, 'progress 更新 total');

// finished 终态；此后迟到 progress 不得拉回下载中（禁止倒流假象）。
row = applyDownloadProgress(row, progressEvent('finished', { bytesDownloaded: 4096 }));
equal(row.phase, 'finished', 'finished 终态');
row = applyDownloadProgress(row, progressEvent('progress', { bytesDownloaded: 1 }));
equal(row.phase, 'finished', '终态忽略迟到 progress');

// failed 携带错误文本；cancelled 清错误。
const failed = applyDownloadProgress(
  applyDownloadProgress(idleDownloadRow(), progressEvent('started')),
  progressEvent('failed', { error: 'SHA-256 mismatch' }),
);
equal(failed.phase, 'failed', 'failed 终态');
assert(failed.error?.includes('SHA-256 mismatch') === true, 'failed 携带错误文本');
const cancelled = applyDownloadProgress(
  applyDownloadProgress(idleDownloadRow(), progressEvent('started')),
  progressEvent('cancelled'),
);
equal(cancelled.phase, 'cancelled', 'cancelled 终态');
equal(cancelled.error, null, 'cancelled 无错误文本');

// 格式化与百分比边界。
equal(formatDownloadBytes(4_590_807_392), '4.3 GB', 'GB 格式');
equal(formatDownloadBytes(1_671_853), '1.6 MB', 'MB 格式');
equal(formatDownloadBytes(512), '1 KB', 'KB 下限');
equal(downloadRowPercent(idleDownloadRow()), null, 'idle 无百分比');
equal(
  downloadRowPercent({ ...idleDownloadRow(), phase: 'downloading', bytes: 2048, total: 4096 }),
  50,
  '百分比四舍五入',
);
equal(
  downloadRowPercent({ ...idleDownloadRow(), phase: 'downloading', bytes: 5000, total: 4096 }),
  100,
  '百分比封顶 100',
);

console.log('modelDownloadState.test.ts: ok');
