import { spawnSync } from 'node:child_process';

const result = spawnSync(
  'cargo',
  [
    'test',
    '--locked',
    '-p',
    'openless-core',
    'hotkey_interpreter::tests',
    '--',
    '--nocapture',
  ],
  {
    env: process.env,
    encoding: 'utf8',
  },
);

const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
for (const chunk of (result.stdout ?? '').match(/[\s\S]{1,8192}/g) ?? []) process.stdout.write(chunk);
for (const chunk of (result.stderr ?? '').match(/[\s\S]{1,8192}/g) ?? []) process.stderr.write(chunk);

if (result.status !== 0) {
  if (result.error) console.error(result.error);
  process.exit(result.status ?? 1);
}

// 录音方式收敛为切换式后，Auto/Hold 专用测试随功能一起删除；守门对象改为
// 仍保留的切换式边界测试（终态冷却 + 防抖 + Combined 代次仲裁）。
if (!output.includes('test hotkey_interpreter::tests::terminal_cooldown_drops_issue_545_third_press ... ok')) {
  console.error('Core hotkey edge gate did not execute the expected test.');
  process.exit(1);
}

console.log('Core hotkey edge gate passed.');
