// localModels.ts — 固定本地模型（Qwen3-ASR / Gemma LLM）的真实引擎状态。
//
// 状态来自后端共享实例（预载与听写/润色是同一对象），不会与实际加载分叉：
// - ASR: local_asr_engine_status（worker 真实 loading/loaded/error + 模型在盘检查）
// - LLM: local_llm_model_status（同一个 LocalLlamaPolisher slot 的状态机）
// 订阅 engine-changed 事件即时刷新 + 窗口回前台 + 低频轮询兜底补漏。
//
// 查询失败的语义与「未加载」严格区分：Tauri 运行时里命令失败会得到显式的
// fetchFailed 快照（展示为 failed + 错误短提示），绝不沿用上一轮的 loaded
// 假绿；非 Tauri 预览（无后端）保持 null → notLoaded。

import { useCallback, useEffect, useRef, useState } from 'react';

export type LocalModelState = 'notFound' | 'notLoaded' | 'loading' | 'loaded' | 'failed';

export interface AsrEngineStatus {
  loaded: boolean;
  modelId: string | null;
  keepLoadedSecs: number;
  available: boolean;
  loading: boolean;
  modelPresent: boolean;
  error: string | null;
}

export interface LlmEngineStatus {
  state: LocalModelState;
  fileName: string;
  sizeBytes: number;
  error: string | null;
}

/** 状态查询本身失败（IPC 报错/后端不可达）的显式标记 —— 与后端报告的
 *  notLoaded/failed 是两回事，但对用户都呈现为 failed + 错误提示。 */
export interface EngineFetchError {
  fetchFailed: true;
  error: string;
}

export type AsrEngineFetch = AsrEngineStatus | EngineFetchError | null;
export type LlmEngineFetch = LlmEngineStatus | EngineFetchError | null;

export const ASR_FIXED_NAME = 'Qwen3-ASR 1.7B';
export const ASR_FIXED_PROVIDER = 'local-qwen3-mlx';
export const LLM_FIXED_NAME = 'Gemma 4 E4B';
export const LLM_FIXED_PROVIDER = 'local-llama';

function isFetchError(value: unknown): value is EngineFetchError {
  return (
    typeof value === 'object' &&
    value !== null &&
    (value as Record<string, unknown>).fetchFailed === true
  );
}

export function asrState(status: AsrEngineFetch): LocalModelState {
  if (!status) return 'notLoaded';
  if (isFetchError(status)) return 'failed';
  if (status.loaded) return 'loaded';
  if (status.loading) return 'loading';
  if (!status.modelPresent) return 'notFound';
  if (status.error) return 'failed';
  return 'notLoaded';
}

export function llmState(status: LlmEngineFetch): LocalModelState {
  if (!status) return 'notLoaded';
  if (isFetchError(status)) return 'failed';
  return status.state;
}

/** 卡片错误短提示的数据源（完整错误文本由卡片截断展示 + title 悬浮）。 */
export function asrError(status: AsrEngineFetch): string | null {
  if (!status) return null;
  if (isFetchError(status)) return status.error;
  return status.error;
}

export function llmError(status: LlmEngineFetch): string | null {
  if (!status) return null;
  if (isFetchError(status)) return status.error;
  return status.error;
}

function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 可注入的命令调用（测试用假 invoker 驱动同一套归一逻辑）。 */
export type StatusInvoker = (command: string) => Promise<unknown>;

/** 非 Tauri 环境返回 null：预览无后端，不算失败。 */
async function defaultInvoke(command: string): Promise<unknown> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke(command);
}

export async function fetchEngineStatus<T>(
  command: string,
  invoke: StatusInvoker | null,
): Promise<T | EngineFetchError | null> {
  if (!invoke) return null;
  try {
    return (await invoke(command)) as T;
  } catch (error) {
    console.error(`[local-models] ${command} failed`, error);
    return { fetchFailed: true, error: errorText(error) };
  }
}

export function snapshotInvoker(): StatusInvoker | null {
  return isTauriRuntime() ? defaultInvoke : null;
}

export interface LocalModelsSnapshot {
  asr: AsrEngineFetch;
  llm: LlmEngineFetch;
}

/** 单次刷新：两个引擎并行取数，失败各自显式 failed（不清成 notLoaded，也绝不
 *  保留上一轮的 loaded —— 旧状态随本快照整体替换）。 */
export async function fetchLocalModelSnapshot(
  invoke: StatusInvoker | null = snapshotInvoker(),
): Promise<LocalModelsSnapshot> {
  const [asr, llm] = await Promise.all([
    fetchEngineStatus<AsrEngineStatus>('local_asr_engine_status', invoke),
    fetchEngineStatus<LlmEngineStatus>('local_llm_model_status', invoke),
  ]);
  return { asr, llm };
}

/** 订阅 + 回前台 + 低频轮询的共享状态 hook（同一份后端真值，不写死）。 */
export function useLocalModelStatuses(pollMs = 5000): LocalModelsSnapshot {
  const [snapshot, setSnapshot] = useState<LocalModelsSnapshot>({ asr: null, llm: null });
  const inflight = useRef(false);

  const refresh = useCallback(async () => {
    if (inflight.current) return;
    inflight.current = true;
    try {
      // 整体替换：查询失败 → 显式 failed（带错误文本），预览 → null → notLoaded。
      setSnapshot(await fetchLocalModelSnapshot());
    } finally {
      inflight.current = false;
    }
  }, []);

  useEffect(() => {
    void refresh();
    if (!isTauriRuntime()) return;

    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const listenEvents = async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const add = async (event: string, handler: () => void) => {
        const stop = await listen(event, handler);
        if (disposed) {
          stop();
        } else {
          unlisteners.push(stop);
        }
      };
      // 事件到达即重拉一次命令状态：载荷只当变化信号，展示永远以后端查询为准。
      await add('local-asr:engine-changed', () => void refresh());
      await add('local-asr-prepare-progress', () => void refresh());
      await add('local-llm:engine-changed', () => void refresh());
    };
    void listenEvents();

    const poll = window.setInterval(() => void refresh(), pollMs);
    const onFocus = () => void refresh();
    window.addEventListener('focus', onFocus);
    return () => {
      disposed = true;
      window.clearInterval(poll);
      window.removeEventListener('focus', onFocus);
      unlisteners.forEach(stop => stop());
    };
  }, [refresh, pollMs]);

  return snapshot;
}
