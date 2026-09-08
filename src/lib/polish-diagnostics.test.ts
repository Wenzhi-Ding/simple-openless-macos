// polish-diagnostics.test.ts — 诊断展示逻辑的最小测试（纯函数，无 IPC/无渲染）。
// 与 history-repolish.test.ts 同款裸断言风格，由 scripts/frontend-test-runner.mjs 发现。
import {
  contextStatusLabelKey,
  contextSummary,
  diagnosticsHeadlineParts,
  formatPolishDiagnosticsJson,
  outcomeLabelKey,
  stageLabelKey,
} from './polish-diagnostics';
import type { PolishDiagnostics } from './types';

function assert(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}

function assertEqual<T>(actual: T, expected: T, message: string) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

function assertDeepEqual(actual: unknown, expected: unknown, message: string) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(
      `${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

// label 映射：已知值原样返回 key，未知/缺失值兜底，绝不抛错（旧数据防御）。
assertEqual(stageLabelKey('tokenized'), 'tokenized', 'known stage');
assertEqual(stageLabelKey(null), 'notCalled', 'null stage falls back');
assertEqual(stageLabelKey(undefined), 'notCalled', 'undefined stage falls back');
assertEqual(stageLabelKey('nonsense' as never), 'notCalled', 'unknown stage falls back');
assertEqual(outcomeLabelKey('succeeded'), 'succeeded', 'known outcome');
assertEqual(outcomeLabelKey(null), 'notCalled', 'null outcome falls back');
assertEqual(contextStatusLabelKey('permissionDenied'), 'permissionDenied', 'known status');
assertEqual(contextStatusLabelKey(null), 'unknown', 'null status falls back');
assertEqual(contextStatusLabelKey('weird' as never), 'unknown', 'unknown status falls back');

// 上下文摘要：included 且带正文 → hasText + 字数；缺失条目 → unknown 摘要。
assertDeepEqual(
  contextSummary({ status: 'included', reason: null, text: 'abcd' }),
  { statusKey: 'included', reason: null, hasText: true, textChars: 4 },
  'included summary',
);
assertDeepEqual(
  contextSummary(null),
  { statusKey: 'unknown', reason: null, hasText: false, textChars: 0 },
  'missing summary',
);

// 复制文本：完整 JSON 含 prompt/messages 正文与上下文原因（诊断用途，本机数据）。
const sample: PolishDiagnostics = {
  stage: 'succeeded',
  outcome: 'succeeded',
  provider: 'local-llama',
  model: 'gemma-4-E2B-it-Q4_0.gguf',
  template: 'gemma4Canonical',
  addBos: true,
  addBosNote: 'BOS token is injected…',
  prompt: '<|turn>system\n…\n<|turn>model\n',
  messages: [{ role: 'user', content: 'hi' }],
  promptTokens: 42,
  contextTokens: 16384,
  generationBudget: 1024,
  error: null,
  notCalledReason: null,
  cursor: { status: 'included', reason: null, text: 'doc' },
  screenOcr: { status: 'notReady', reason: 'observation not ready', text: null },
};
const parsed = JSON.parse(formatPolishDiagnosticsJson(sample)) as PolishDiagnostics;
assertEqual(parsed.prompt, sample.prompt, 'copied json keeps the prompt');
assertEqual(parsed.screenOcr?.reason ?? null, 'observation not ready', 'copied json keeps reasons');
assertDeepEqual(parsed.messages, sample.messages, 'copied json keeps messages');

// 概览行：模型名 + token 数 + 预算；缺失字段自动省略。
assertDeepEqual(
  diagnosticsHeadlineParts(sample),
  ['gemma-4-E2B-it-Q4_0.gguf', '42 tok', 'budget 1024'],
  'headline parts',
);
assertDeepEqual(
  diagnosticsHeadlineParts({ ...sample, model: null, promptTokens: null, generationBudget: null }),
  [],
  'empty headline',
);

console.log('[polish-diagnostics.test] all assertions passed');
