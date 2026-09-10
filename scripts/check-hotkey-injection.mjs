import { spawnSync } from 'node:child_process';

const result = spawnSync(
  'cargo',
  [
    'test',
    '--locked',
    '-p',
    'openless-core',
    'hotkey_interpreter::tests::auto_uses_native_press_duration_and_release_generation',
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

if (!output.includes('test hotkey_interpreter::tests::auto_uses_native_press_duration_and_release_generation ... ok')) {
  console.error('Core hotkey edge gate did not execute the expected test.');
  process.exit(1);
}

console.log('Core hotkey edge gate passed.');
