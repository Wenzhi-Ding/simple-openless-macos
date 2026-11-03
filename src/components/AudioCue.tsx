// 录音提示音：监听 capsule:state 事件，在"开始录音"边沿播放合成提示音。
// 独立组件，不依赖胶囊窗口显示——胶囊隐藏时也能正常工作。

import { useEffect, useRef } from 'react';
import { isTauri } from '../lib/ipc';
import {
  cueEdgeAction,
  cueStateFromDictationPhase,
  playRecordStartCue,
  primeAudioCue,
  stopAudioCue,
} from '../lib/audioCue';
import type { CapsuleState, UserPreferences } from '../lib/types';

interface CapsulePayload {
  state: CapsuleState;
  level?: number;
  message?: string | null;
  insertedChars?: number | null;
  translation?: boolean;
}

export function AudioCueListener() {
  const audioCueEnabledRef = useRef<boolean>(true);
  // 初始 'idle' 只是订阅建立前的占位值；下面的监听 effect 会在订阅前用后端当前听写
  // 快照校准它，所以录音进行中重挂载时不会被误判成「刚开始录音」。
  const prevStateRef = useRef<CapsuleState>('idle' as CapsuleState);
  // 读取设置（默认开启）
  useEffect(() => {
    if (!isTauri) return;
    let cancelled = false;
    (async () => {
      try {
        const { getSettings } = await import('../lib/ipc');
        const prefs = await getSettings();
        if (!cancelled) audioCueEnabledRef.current = prefs.audioCueOnRecord !== false;
      } catch {
        // 读取失败保持默认 true
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // 监听设置变更
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      listen<UserPreferences>('prefs:changed', (event) => {
        const next = event.payload;
        if (next) audioCueEnabledRef.current = next.audioCueOnRecord !== false;
      }).then(fn => { if (cancelled) fn(); else unlisten = fn; }).catch(() => {});
    })();
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // 预热 AudioContext
  useEffect(() => {
    if (!isTauri) return;
    primeAudioCue();
  }, []);

  // 监听 capsule 状态边沿
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      const [{ listen }, { getCurrentDictationPhase }] = await Promise.all([
        import('@tauri-apps/api/event'),
        import('../lib/ipc'),
      ]);
      if (cancelled) return;
      // 先向后端要当前听写状态做基线，再订阅：基线必须早于任何实时事件，否则录音进行中
      // 重挂载时首条 recording 会被当成「刚开始录音」而误放提示音。正常启动时基线是
      // idle，第一次 idle → recording 照常播放。代价是这次 IPC 往返（毫秒级）内发生的
      // 事件收不到，而该窗口内不存在需要播放的开始边沿。
      try {
        prevStateRef.current = cueStateFromDictationPhase(await getCurrentDictationPhase());
      } catch (error) {
        console.warn('[audio-cue] 读取当前听写状态失败，按 idle 建立基线', error);
        prevStateRef.current = 'idle';
      }
      if (cancelled) return;
      listen<CapsulePayload>('capsule:state', (event) => {
        const state = event.payload.state;
        const prev = prevStateRef.current;
        prevStateRef.current = state;
        const edge = cueEdgeAction(prev, state);
        if (edge === 'play') {
          if (audioCueEnabledRef.current) playRecordStartCue();
        } else if (edge === 'stop') {
          stopAudioCue();
        }
      }).then(fn => { if (cancelled) fn(); else unlisten = fn; }).catch(() => {});
    })();
    return () => {
      cancelled = true;
      unlisten?.();
      // 卸载时若还有提示音在排期或播放，只 unlisten 会让它继续响完；这里停掉在播节点，
      // 并记一次 stop——挂起 resume 的补播若已超过迟到阈值就丢弃（阈值内仍按原设计补响）。
      stopAudioCue();
    };
  }, []);

  return null;
}
