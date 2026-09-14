import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const read = relativePath => readFileSync(join(appRoot, relativePath), 'utf8');

// 布局偏好（易读布局 / 保守排版）已删除，换行只跟随窄窗口断点。本文件保留其中
// 仍然有效的回归守卫：CSS 选择器约束、行组件的堆叠行为、历史页无风格系统。
const css = read('src/styles/global.css');
const shared = read('src/pages/settings/shared.tsx');
const pageAtoms = read('src/pages/_atoms.tsx');
const row = read('src/components/ui/Row.tsx');
const permissions = read('src/pages/settings/PermissionsSection.tsx');
const shell = read('src/components/FloatingShell.tsx');
assert.doesNotMatch(shell, /id:\s*'style'/);

assert.doesNotMatch(css, /\.ol-flex-row\s*>\s*\*/);
assert.doesNotMatch(css, /\[class\*=["']ol-grid["']\]/);

assert.match(row, /stackLayout\s*\?\s*'minmax\(0, 1fr\)'/);

assert.match(permissions, /permissionActionsStyle/);
assert.match(permissions, /justifyContent:\s*stackLayout\s*\?\s*'flex-start'\s*:\s*'flex-end'/);

const history = read('src/pages/History.tsx');
const overview = read('src/pages/Overview.tsx');
for (const source of [history, overview]) {
  assert.doesNotMatch(source, /style\.modes|styleLabel|modeLabel|FILTERS|setFilter|listStylePacks|stylePackId/);
  assert.doesNotMatch(source, /(?:session|item|s)\.mode\b/);
}
assert.match(history, /history\.rawLabel/);
assert.match(history, /history\.resultLabel/);
assert.match(history, /repolish\(session\.rawTranscript\)/);
assert.match(history, /diagnostics\.prompt/);
assert.match(history, /readAudioRecording/);
for (const locale of ['zh-CN', 'zh-TW', 'en', 'ko']) {
  const source = read(`src/i18n/${locale}.ts`);
  const hint = source.match(/["']repolish["']\s*:\s*\{[\s\S]*?["']hint["']\s*:\s*["']([^"']*)/)?.[1];
  assert.ok(hint);
  assert.doesNotMatch(hint, /风格|風格|style|스타일/);
}
console.log('layout mode and style-free history contract tests passed');
