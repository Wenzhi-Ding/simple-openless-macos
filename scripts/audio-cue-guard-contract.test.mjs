import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const read = relativePath => readFileSync(join(appRoot, relativePath), 'utf8');

const listener = read('src/components/AudioCue.tsx');
const shell = read('src/components/FloatingShell.tsx');
const app = read('src/App.tsx');
const ipcDictation = read('src/lib/ipc/dictation.ts');
const cueBackend = read('src-tauri/src/audio_cue.rs');
const settingsSection = read('src/pages/settings/RecordingInputSection.tsx');

// 监听器留在主窗（FloatingShell）。上游 ed96c558 把监听器移进 capsule 窗口，是配合
// 「销毁主窗 WebView」的另一项改动；本项目主窗只 hide、不销毁，不采纳该改动。
assert.match(shell, /<AudioCueListener \/>/);
assert.doesNotMatch(app, /AudioCueListener/);

// 发声必须走宿主进程 IPC，不得回到 Web Audio：WKWebView 渲染进程的音频输出通道会
// 在多次听写后不可探测地卡死（2026-11-03 结构性偏离，见 docs/upstream-sync.md）。
assert.doesNotMatch(listener, /AudioContext|playRecordStartCue|primeAudioCue|stopAudioCue/);
assert.match(listener, /playCue\('start'\)/);
assert.match(listener, /playCue\('stop'\)/);
assert.match(ipcDictation, /play_cue/);
assert.match(ipcDictation, /stop_cue/);
// 两端音符参数镜像：前端不再持参数，后端持有镜像双音。
assert.match(cueBackend, /1108\.73/);
// 设置页试听与录音路径同一发声通道。
assert.match(settingsSection, /playCue\('start'\)/);

const stateEffect = listener.match(/\/\/ 监听 capsule 状态边沿[\s\S]*?\n  \}, \[\]\);/);
assert.ok(stateEffect, '找不到监听 capsule:state 的 effect');
const effectSource = stateEffect[0];

// 守卫一：卸载清理不能只 unlisten，还要通知宿主截断在播提示音。
const cleanup = effectSource.match(/return \(\) => \{[\s\S]*?\n    \};/);
assert.ok(cleanup, '找不到 capsule:state effect 的清理函数');
assert.match(cleanup[0], /unlisten\?\.\(\);/);
assert.match(cleanup[0], /stopCue\(\)/);

// 守卫二：订阅前必须先用后端当前听写快照建立基线，且不能把首条事件当基线吞掉。
// 顺序是关键——先定基线再订阅，重挂载时首条 recording 才不会被误判成开始；正常启动的
// 第一次 idle → recording 仍要播放（边沿判定见 src/lib/audioCue.test.ts）。
const baselineIndex = effectSource.search(
  /cueStateFromDictationPhase\(\s*await getCurrentDictationPhase\(\)\s*\)/,
);
const listenIndex = effectSource.indexOf("listen<CapsulePayload>('capsule:state'");
assert.ok(baselineIndex > -1, '找不到用后端快照建立基线的代码');
assert.ok(listenIndex > -1, '找不到 capsule:state 订阅');
assert.ok(baselineIndex < listenIndex, '基线必须建立在订阅之前');
assert.match(effectSource, /cueEdgeAction\(prev, state\)/);
assert.doesNotMatch(listener, /hasBaselineRef/);

console.log('audio cue guards contract test passed');
