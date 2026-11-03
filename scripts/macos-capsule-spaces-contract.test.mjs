import { readFile } from 'node:fs/promises';

function assertMatch(source, pattern, name) {
  if (!pattern.test(source)) {
    throw new Error(`${name}: pattern ${pattern} not found`);
  }
}

// 契约函数 show_capsule_window_no_activate 位于显式 Tauri Host Module。
// 契约必须校验真正编译的那份，否则会出现
// 「测试绿、线上坏」的假信心。
const capsuleFocusRs = (
  await readFile(new URL('../src-tauri/src/coordinator/capsule_focus.rs', import.meta.url), 'utf-8')
).replace(/\r\n/g, '\n');
const coordinatorHostRs = (
  await readFile(new URL('../src-tauri/src/tauri_coordinator_host.rs', import.meta.url), 'utf-8')
).replace(/\r\n/g, '\n');
const functionMatch = coordinatorHostRs.match(
  /(?:pub\((?:crate|super)\) )?fn show_capsule_window_no_activate[\s\S]*?\n}\n(?=\S)/,
);

if (!functionMatch) {
  throw new Error('macOS capsule no-activate function not found');
}

const macosNoActivateFunction = functionMatch[0];
const executableMacosNoActivateFunction = macosNoActivateFunction.replace(/\/\/.*$/gm, '');

// 2026-11-03 起（46a86b29 线程纪律收编），裸位值抽成了模块常量并加了漂移检测重注册；
// 行为不变：函数仍经命名常量做绝对 setCollectionBehavior 写入，然后 orderFrontRegardless。
assertMatch(
  coordinatorHostRs,
  /const CAN_JOIN_ALL_SPACES: usize = 1 << 0;/,
  'macOS capsule collection behavior constants must keep CAN_JOIN_ALL_SPACES = 1 << 0',
);

assertMatch(
  coordinatorHostRs,
  /const FULL_SCREEN_AUXILIARY: usize = 1 << 8;/,
  'macOS capsule collection behavior constants must keep FULL_SCREEN_AUXILIARY = 1 << 8',
);

assertMatch(
  macosNoActivateFunction,
  /setCollectionBehavior[\s\S]*?orderFrontRegardless/,
  'macOS capsule should join all Spaces via an absolute collectionBehavior write before showing without activation',
);

assertMatch(
  macosNoActivateFunction,
  /setCollectionBehavior : CAPSULE_COLLECTION_BEHAVIOR/,
  'macOS capsule must write the full CAN_JOIN_ALL_SPACES|STATIONARY|FULL_SCREEN_AUXILIARY combination, not a partial mask',
);

assertMatch(
  macosNoActivateFunction,
  /setLevel\s*:\s*25[\s\S]*?orderFrontRegardless/,
  'macOS capsule must raise window level above the menu bar (25) so it renders over fullscreen apps, not just behind them',
);

for (const forbidden of ['window.show()', 'set_focus', 'NSApp.activate', 'makeKeyAndOrderFront']) {
  if (executableMacosNoActivateFunction.includes(forbidden)) {
    throw new Error(`macOS capsule no-activate path must not call ${forbidden}`);
  }
}

// === 胶囊跟随「鼠标光标所在屏」契约（多屏 / 多 Space）===
// 根因：定位用 AX caret、layout 去重缓存却用胶囊自己的 current_monitor，两者看
// 不同的屏；光标移到另一块屏时缓存误判「没变化」→ 跳过重新定位 → 胶囊被锁死
// 在第一块屏（别屏只闪一下）。修复后两条路径必须共用 capsule_target_monitor，
// 且以鼠标光标为首选信号。这些不变量纯靠源码 grep 守护，无法在无多屏硬件的
// 单测里覆盖，正是契约测试的用武之地。
const libRs = (
  await readFile(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf-8')
).replace(/\r\n/g, '\n');

assertMatch(
  libRs,
  /fn capsule_target_monitor[\s\S]*?macos_mouse_cursor_point\(\)\s*\.or_else\(\s*macos_focused_input_anchor_point\s*\)/,
  'macOS capsule must resolve its target monitor from the mouse cursor first, AX caret only as fallback',
);

assertMatch(
  libRs,
  /position_capsule_bottom_center[\s\S]*?if let Some\(mon\) = capsule_target_monitor\(window\)/,
  'macOS capsule positioning must follow capsule_target_monitor (the mouse screen), not its own current_monitor',
);

assertMatch(
  coordinatorHostRs,
  /crate::capsule_target_monitor\(window\)/,
  'macOS capsule layout cache key must reuse capsule_target_monitor, or it will skip repositioning when the cursor moves to another screen',
);
