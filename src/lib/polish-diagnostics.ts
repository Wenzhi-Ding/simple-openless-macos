// polish-diagnostics.ts — 润色诊断（polishDiagnostics）的纯展示逻辑。
//
// 后端把「这次实际送入润色模型的完整 prompt」内嵌进 history.json（用户已授权，
// 仅本机）。本文件只做 label 映射与复制文本格式化，不触碰 IPC；UI 组件
// （History 页的折叠区）消费这里的 helper，测试直接覆盖本文件。

import type {
  PolishContextDiagnostics,
  PolishContextStatus,
  PolishDiagnostics,
  PolishDiagnosticsOutcome,
  PolishDiagnosticsStage,
} from './types';

/** 阶段 → i18n key（history.diagnostics.stage*）。未知值兜底 notCalled。 */
export function stageLabelKey(stage: PolishDiagnosticsStage | null | undefined): string {
  const known: ReadonlySet<string> = new Set([
    'notCalled', 'modelMissing', 'modelLoadFailed', 'renderFailed',
    'rendered', 'tokenized', 'prefilled', 'generating', 'succeeded',
  ]);
  const value = stage ?? 'notCalled';
  return known.has(value) ? value : 'notCalled';
}

/** 结局 → i18n key（history.diagnostics.outcome*）。 */
export function outcomeLabelKey(outcome: PolishDiagnosticsOutcome | null | undefined): string {
  const value = outcome ?? 'notCalled';
  return value === 'succeeded' || value === 'failed' ? value : 'notCalled';
}

/** 上下文结局 → i18n key（history.diagnostics.status*）。未知值兜底 unknown。 */
export function contextStatusLabelKey(status: PolishContextStatus | null | undefined): string {
  const known: ReadonlySet<string> = new Set([
    'included', 'notAdopted', 'disabled', 'notSupported', 'notStarted',
    'permissionDenied', 'sensitiveBlocked', 'targetChanged', 'timeout',
    'notReady', 'empty', 'failed', 'blocked', 'unavailable', 'unknown',
  ]);
  const value = status ?? 'unknown';
  return known.has(value) ? value : 'unknown';
}

/** 单个上下文条目的一行摘要文本（供折叠区头部；正文只在展开后渲染）。 */
export function contextSummary(
  context: PolishContextDiagnostics | null | undefined,
): { statusKey: string; reason: string | null; hasText: boolean; textChars: number } {
  if (!context) {
    return { statusKey: 'unknown', reason: null, hasText: false, textChars: 0 };
  }
  return {
    statusKey: contextStatusLabelKey(context.status),
    reason: context.reason,
    hasText: context.text != null && context.text.length > 0,
    textChars: context.text?.length ?? 0,
  };
}

/** 复制完整诊断：结构化 JSON（含 prompt 与消息正文，供粘贴到别处排查）。 */
export function formatPolishDiagnosticsJson(diagnostics: PolishDiagnostics): string {
  return JSON.stringify(diagnostics, null, 2);
}

/** 概览行文本（模型 · token · 预算），供折叠区标题旁的简报。 */
export function diagnosticsHeadlineParts(diagnostics: PolishDiagnostics): string[] {
  const parts: string[] = [];
  if (diagnostics.model) parts.push(diagnostics.model);
  if (diagnostics.promptTokens != null) parts.push(`${diagnostics.promptTokens} tok`);
  if (diagnostics.generationBudget != null) parts.push(`budget ${diagnostics.generationBudget}`);
  return parts;
}
