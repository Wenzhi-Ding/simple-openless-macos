// 录音提示音的「时机」判定层：按 capsule 状态边沿决定何时播放/停止。
// 「发声」在宿主进程（src-tauri/src/audio_cue.rs，cpal 输出流合成），前端只通过
// IPC 触发，不再走 Web Audio：WKWebView 渲染进程的音频输出通道会在多次听写后
// 卡死——卡死后 AudioContext.state 照常 running、节点照常「正常结束」，网页侧
// 既探测不到也无法恢复，只有重启进程才恢复（2026-11-03 结构性偏离，见
// docs/upstream-sync.md）。本文件剩下的都是纯函数，可被单测钉住。

import type { CapsuleState } from './types';

/** 一次 capsule 状态迁移对提示音该做什么。 */
export type CueEdgeAction = 'play' | 'stop' | 'none';

/**
 * 按前后状态判定提示音边沿。纯函数，便于单测。
 * prev 是「监听器挂载时从后端快照取到的真实状态」或上一条事件的状态：
 * 正常启动时快照是 idle，第一次 idle → recording 必须返回 'play'；录音进行中重挂载时
 * 快照已是 recording，首条 recording 事件返回 'none'，不重放开始提示音。
 */
export function cueEdgeAction(prev: CapsuleState, next: CapsuleState): CueEdgeAction {
  if (next === 'recording' && prev !== 'recording') return 'play';
  if (next !== 'recording' && prev === 'recording') return 'stop';
  return 'none';
}

/**
 * 把后端听写阶段（camelCase，来自 StartupSnapshot.backend.dictation.phase）映射成提示音
 * 使用的胶囊状态。映射与 src-tauri/src/tauri_events.rs 的 map_dictation_state 一致。
 * 监听器挂载时用它把 prevStateRef 初始化成后端真实状态：录音进行中重挂载时首条
 * recording 就不会被当成「刚开始录音」而误放提示音；正常启动后第一次录音则是真实的
 * idle → recording 边沿，照常播放。
 */
export function cueStateFromDictationPhase(phase: string): CapsuleState {
  switch (phase) {
    case 'starting':
    case 'recording':
      return 'recording';
    case 'transcribing':
      return 'transcribing';
    case 'polishing':
    case 'inserting':
      return 'polishing';
    case 'completed':
      return 'done';
    case 'cancelled':
      return 'cancelled';
    case 'failed':
      return 'error';
    default:
      return 'idle';
  }
}
