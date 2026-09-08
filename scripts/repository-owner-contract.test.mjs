import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('..', import.meta.url));
const legacyRepo = 'appergb/openless';

const repositoryReferences = [
  'upstream/.github/ISSUE_TEMPLATE/config.yml',
  'upstream/.github/workflows/android-apk.yml',
  'upstream/.github/workflows/release-tauri.yml',
  'upstream/Casks/openless.rb',
  'upstream/README.md',
  'upstream/README.zh.md',
  'upstream/USAGE.md',
  'src-tauri/src/android/updater_logic.rs',
  'src-tauri/src/commands/mod.rs',
  'src-tauri/src/commands/settings.rs',
  'src-tauri/tauri.conf.json',
  'src/components/SettingsModal.tsx',
  'src/pages/settings/AboutSection.tsx',
  'scripts/write-updater-manifest.mjs',
];

for (const relativePath of repositoryReferences) {
  const content = await readFile(join(repoRoot, relativePath), 'utf8');
  assert(!content.includes(legacyRepo), `${relativePath} still references the pre-transfer repository`);
}

console.log('repository-owner-contract.test.mjs passed');
