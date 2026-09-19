// localModels.test.ts — 状态映射与「查询失败清旧状态」的最小行为契约。
// 不测性能/轮询节奏，只锁定：失败必须是显式 failed（不是 notLoaded）、
// 上一轮的 loaded 不得在失败后残留、loaded/loading 按后端真值映射、
// 非 Tauri 预览保持 null → notLoaded。

import {
  ASR_MEMORY_ESTIMATE_BYTES,
  LLM_FIXED_NAME,
  LLM_FIXED_PROVIDER,
  LLM_MEMORY_ESTIMATE_BYTES,
  OCR_FIXED_NAME,
  OCR_FIXED_PROVIDER,
  OCR_MEMORY_ESTIMATE_BYTES,
  asrError,
  asrState,
  fetchEngineStatus,
  fetchLocalModelSnapshot,
  formatMemoryBytes,
  llmError,
  llmState,
  ocrError,
  ocrState,
  type AsrEngineStatus,
  type LlmEngineStatus,
  type OcrEngineStatus,
  type StatusInvoker,
} from './localModels';


function assert(condition: boolean, message = 'condition must hold') {
  if (!condition) throw new Error(message);
}

function equal<T>(actual: T, expected: T, message = 'values must be equal') {
  assert(
    actual === expected,
    `${message}: expected ${String(expected)}, got ${String(actual)}`,
  );
}

function asrStatus(overrides: Partial<AsrEngineStatus> = {}): AsrEngineStatus {
  return {
    loaded: false,
    modelId: null,
    keepLoadedSecs: 300,
    available: true,
    loading: false,
    modelPresent: true,
    error: null,
    ...overrides,
  };
}

function ocrStatus(overrides: Partial<OcrEngineStatus> = {}): OcrEngineStatus {
  return {
    runtimePresent: true,
    loading: false,
    workerAlive: false,
    lastError: null,
    ...overrides,
  };
}

// ─── loaded / loading / notFound / failed 映射 ─────────────────────────────

equal(LLM_FIXED_NAME, 'Gemma 4 E4B');
equal(LLM_FIXED_PROVIDER, 'local-llama');
equal(OCR_FIXED_NAME, 'RapidOCR');
equal(OCR_FIXED_PROVIDER, 'local-rapidocr');
equal(asrState(asrStatus({ loaded: true })), 'loaded');
equal(asrState(asrStatus({ loading: true })), 'loading');
// loading 优先级低于 loaded：worker 已就绪就不再展示加载中。
equal(asrState(asrStatus({ loaded: true, loading: true })), 'loaded');
equal(asrState(asrStatus({ modelPresent: false })), 'notFound');
equal(asrState(asrStatus({ error: 'worker failed to load model' })), 'failed');
equal(asrState(asrStatus()), 'notLoaded');
equal(asrState(null), 'notLoaded');

equal(llmState({ state: 'loaded', fileName: 'gemma-4-E4B-it-Q4_0.gguf', sizeBytes: 1, error: null }), 'loaded');
equal(llmState({ state: 'loading', fileName: 'g', sizeBytes: 1, error: null }), 'loading');
equal(llmState({ state: 'notFound', fileName: 'g', sizeBytes: 0, error: null }), 'notFound');
equal(llmState({ state: 'failed', fileName: 'g', sizeBytes: 1, error: 'boom' }), 'failed');
equal(llmState(null), 'notLoaded');

// ─── OCR 状态映射：运行时缺失 / 冷启动 / 存活 / 失败 / 未拉起 ────────────────

equal(ocrState(ocrStatus({ workerAlive: true })), 'loaded');
equal(ocrState(ocrStatus({ loading: true })), 'loading');
// loading 优先级低于存活：worker 已就绪就不再展示加载中。
equal(ocrState(ocrStatus({ loading: true, workerAlive: true })), 'loaded');
equal(ocrState(ocrStatus({ runtimePresent: false })), 'notFound');
// worker 已被丢弃但留有失败记录 → failed；无记录 → notLoaded。
equal(ocrState(ocrStatus({ lastError: 'ocr worker response timeout' })), 'failed');
equal(ocrState(ocrStatus()), 'notLoaded');
equal(ocrState(null), 'notLoaded');

// ─── 内存估算：静态常量的展示文本（估算是写死值，不随后端状态变化） ───────────

equal(formatMemoryBytes(ASR_MEMORY_ESTIMATE_BYTES), '4.0 GB');
equal(formatMemoryBytes(LLM_MEMORY_ESTIMATE_BYTES), '4.3 GB');
equal(formatMemoryBytes(OCR_MEMORY_ESTIMATE_BYTES), '1.3 GB');
equal(formatMemoryBytes(166_400_000), '159 MB');
equal(formatMemoryBytes(0), '0 MB');

// ─── 查询失败 → 显式 failed（不是 notLoaded）──────────────────────────────

const failing: StatusInvoker = async () => {
  throw new Error('IPC bridge closed');
};

const asrFetchError = await fetchEngineStatus<AsrEngineStatus>(
  'local_asr_engine_status',
  failing,
);
assert(
  asrFetchError !== null && 'fetchFailed' in asrFetchError,
  '失败的查询必须返回显式 fetchFailed 标记',
);
equal(asrState(asrFetchError), 'failed');
equal(asrError(asrFetchError), 'IPC bridge closed');

const llmFetchError = await fetchEngineStatus<LlmEngineStatus>(
  'local_llm_model_status',
  failing,
);
equal(llmState(llmFetchError), 'failed');
equal(llmError(llmFetchError), 'IPC bridge closed');

const ocrFetchError = await fetchEngineStatus<OcrEngineStatus>(
  'local_ocr_engine_status',
  failing,
);
equal(ocrState(ocrFetchError), 'failed');
equal(ocrError(ocrFetchError), 'IPC bridge closed');

// 非 Tauri 预览：invoker 为 null → null（未加载，不算失败）。
equal(await fetchEngineStatus('local_asr_engine_status', null), null);
equal(asrState(await fetchEngineStatus('local_asr_engine_status', null)), 'notLoaded');

// ─── 失败清旧状态：上一轮 loaded 不得残留 ──────────────────────────────────

const loadedSnapshot = {
  asr: asrStatus({ loaded: true, modelId: 'qwen3-asr-1.7b' }),
  llm: { state: 'loaded', fileName: 'gemma-4-E4B-it-Q4_0.gguf', sizeBytes: 2_000_000_000, error: null } as LlmEngineStatus,
  ocr: ocrStatus({ workerAlive: true }),
};
equal(asrState(loadedSnapshot.asr), 'loaded');
equal(llmState(loadedSnapshot.llm), 'loaded');
equal(ocrState(loadedSnapshot.ocr), 'loaded');

// 下一轮刷新整体替换：查询失败后快照里不允许还有上一轮的 loaded。
const next = await fetchLocalModelSnapshot(failing);
equal(asrState(next.asr), 'failed', '失败的 ASR 查询必须清掉旧 loaded（禁止假绿）');
equal(llmState(next.llm), 'failed', '失败的 LLM 查询必须清掉旧 loaded（禁止假绿）');
equal(ocrState(next.ocr), 'failed', '失败的 OCR 查询必须清掉旧 loaded（禁止假绿）');
assert(
  asrError(next.asr)?.includes('IPC bridge closed') === true,
  '失败快照必须携带可读错误文本',
);

// 预览（无后端）同样整体替换为 null → 未加载，而不是失败。
const preview = await fetchLocalModelSnapshot(null);
equal(preview.asr, null);
equal(preview.llm, null);
equal(preview.ocr, null);
equal(asrState(preview.asr), 'notLoaded');
equal(ocrState(preview.ocr), 'notLoaded');

console.log('localModels.test.ts: ok');
