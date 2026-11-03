import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const read = relativePath => readFileSync(join(appRoot, relativePath), 'utf8');

const listener = read('src/components/AudioCue.tsx');
const shell = read('src/components/FloatingShell.tsx');
const app = read('src/App.tsx');

// 监听器留在主窗（FloatingShell）。上游 ed96c558 把监听器移进 capsule 窗口，是配合
// 「销毁主窗 WebView」的另一项改动；本项目主窗只 hide、不销毁，不采纳该改动。
assert.match(shell, /<AudioCueListener \/>/);
assert.doesNotMatch(app, /AudioCueListener/);

const stateEffect = listener.match(/\/\/ 监听 capsule 状态边沿[\s\S]*?\n  \}, \[\]\);/);
assert.ok(stateEffect, '找不到监听 capsule:state 的 effect');
const effectSource = stateEffect[0];

// 守卫一：卸载清理不能只 unlisten，还要停掉正在播的提示音。
const cleanup = effectSource.match(/return \(\) => \{[\s\S]*?\n    \};/);
assert.ok(cleanup, '找不到 capsule:state effect 的清理函数');
assert.match(cleanup[0], /unlisten\?\.\(\);/);
assert.match(cleanup[0], /stopAudioCue\(\);/);

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
