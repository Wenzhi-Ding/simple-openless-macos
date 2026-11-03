// audioCue 纯函数单测，沿用仓库现有 .test.ts 的轻量自执行断言风格
// （无独立 runner —— 在 tsc 类型检查下编译，必要时可用 tsx 直接跑）。
// 发声已移到宿主进程（src-tauri/src/audio_cue.rs），音符参数与包络的测试随迁到
// Rust 侧（cargo test）；这里只钉住「何时响」的边沿判定与状态映射。

import {
  cueEdgeAction,
  cueStateFromDictationPhase,
} from './audioCue';

function assertEqual<T>(actual: T, expected: T, name: string) {
  if (actual !== expected) {
    throw new Error(`${name}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

{
  // 场景一：正常启动。挂载时后端快照是 idle，第一次 idle → recording 必须播放提示音；
  // 不能把挂载后第一条事件当成「只同步基线」吞掉。
  assertEqual(
    cueEdgeAction(cueStateFromDictationPhase('idle'), 'recording'),
    'play',
    'first recording after a fresh mount plays the start cue',
  );
  // 场景二：录音进行中重挂载。挂载时后端快照已是 recording，首条 recording 事件不是
  // 新的开始边沿，不能重放开始提示音。
  assertEqual(
    cueEdgeAction(cueStateFromDictationPhase('recording'), 'recording'),
    'none',
    'remount during recording does not replay the start cue',
  );
  assertEqual(
    cueEdgeAction(cueStateFromDictationPhase('starting'), 'recording'),
    'none',
    'remount during capture warmup does not replay the start cue',
  );
  // 重挂载后接着收到离开录音的事件 → 仍然正常收尾。
  assertEqual(
    cueEdgeAction(cueStateFromDictationPhase('recording'), 'transcribing'),
    'stop',
    'remount during recording still stops on the next state',
  );
  // 已在录音中重复收到 recording（后端状态重发）不该叠响第二声。
  assertEqual(
    cueEdgeAction('recording', 'recording'),
    'none',
    'repeated recording state does not replay the cue',
  );
  // 离开录音 → 停止（正常结束放下行结束音；异常退出截断在播音，见 AudioCue.tsx）。
  assertEqual(
    cueEdgeAction('recording', 'transcribing'),
    'stop',
    'leaving recording stops the cue',
  );
  // 非录音状态之间的迁移与提示音无关。
  assertEqual(cueEdgeAction('idle', 'transcribing'), 'none', 'non-recording transitions are silent');
  assertEqual(cueEdgeAction('done', 'recording'), 'play', 'a later recording plays again');
}

{
  // 后端 DictationPhase → 胶囊状态，必须与 src-tauri/src/tauri_events.rs 的
  // map_dictation_state 一致，否则挂载基线会算错边沿。
  assertEqual(cueStateFromDictationPhase('idle'), 'idle', 'idle maps to idle');
  assertEqual(cueStateFromDictationPhase('starting'), 'recording', 'starting maps to recording');
  assertEqual(cueStateFromDictationPhase('recording'), 'recording', 'recording maps to recording');
  assertEqual(
    cueStateFromDictationPhase('transcribing'),
    'transcribing',
    'transcribing maps to transcribing',
  );
  assertEqual(cueStateFromDictationPhase('polishing'), 'polishing', 'polishing maps to polishing');
  assertEqual(cueStateFromDictationPhase('inserting'), 'polishing', 'inserting maps to polishing');
  assertEqual(cueStateFromDictationPhase('completed'), 'done', 'completed maps to done');
  assertEqual(cueStateFromDictationPhase('cancelled'), 'cancelled', 'cancelled maps to cancelled');
  assertEqual(cueStateFromDictationPhase('failed'), 'error', 'failed maps to error');
  // 未知值保守当 idle：宁可照常提示，也不把真实开始边沿吞掉。
  assertEqual(cueStateFromDictationPhase('some-future-phase'), 'idle', 'unknown phase falls back to idle');
}

// 静默成功难以与「没跑」区分；直接 tsx 跑时给个明确通过信号。
console.log('[audioCue.test] all assertions passed');
