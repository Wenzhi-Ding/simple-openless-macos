import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const packageJson = JSON.parse(
  await readFile(new URL('../package.json', import.meta.url), 'utf8'),
);
const packageLock = JSON.parse(
  await readFile(new URL('../package-lock.json', import.meta.url), 'utf8'),
);
const lockRoot = packageLock.packages?.[''];

assert.ok(lockRoot, 'package-lock.json must contain the root package entry');
assert.deepEqual(
  lockRoot.dependencies,
  packageJson.dependencies,
  'package-lock root dependencies must match package.json',
);
assert.deepEqual(
  lockRoot.devDependencies,
  packageJson.devDependencies,
  'package-lock root devDependencies must match package.json',
);

for (const removed of [
  '@shadcn/react',
  '@tailwindcss/vite',
  '@tauri-apps/plugin-updater',
  'dompurify',
  'marked',
  'tailwindcss',
]) {
  assert.equal(
    packageLock.packages[`node_modules/${removed}`],
    undefined,
    `${removed} must be absent from the lockfile`,
  );
}

console.log('package-lock-root-contract.test.mjs passed');
