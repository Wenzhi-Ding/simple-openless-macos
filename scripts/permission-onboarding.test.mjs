import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
const read = path => readFileSync(new URL(`../src/${path}`, import.meta.url), 'utf8');
const compiled = ts.transpileModule(read('lib/permissionOnboarding.ts'), { compilerOptions: { module: ts.ModuleKind.ES2022 } }).outputText;
const { dictationPermissionsReady, screenPermissionNeedsReview, checkPermission, permissionAction, permissionStatusKey, permissionRefreshQueue } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);
assert.equal(dictationPermissionsReady('granted', 'granted'), true);
assert.equal(dictationPermissionsReady('granted', 'noDevice'), true);
assert.equal(dictationPermissionsReady('denied', 'granted'), false);
assert.equal(dictationPermissionsReady('granted', 'denied'), false);
for (const status of ['denied', 'restricted', 'notDetermined', 'checkFailed', 'unavailable']) {
  assert.equal(screenPermissionNeedsReview(true, false, status), true);
  assert.equal(screenPermissionNeedsReview(true, true, status), false);
  assert.equal(screenPermissionNeedsReview(false, false, status), false);
}
for (const status of ['granted', 'notApplicable', 'loading']) assert.equal(permissionAction(status), 'none');
for (const status of ['checkFailed', 'unavailable', 'noDevice']) assert.equal(permissionAction(status), 'retry');
for (const status of ['denied', 'restricted', 'notDetermined']) assert.equal(permissionAction(status), 'request');
assert.equal(permissionStatusKey('unavailable'), 'checkFailed');
let failed = true;
const microphone = () => failed ? Promise.reject(new Error('do not log private error payload')) : Promise.resolve('granted');
assert.deepEqual(await Promise.all([
  checkPermission('accessibility', async () => 'granted', 'test'),
  checkPermission('microphone', microphone, 'test'),
  checkPermission('screenCapture', async () => 'granted', 'test'),
]), ['granted', 'checkFailed', 'granted']);
failed = false;
assert.equal(await checkPermission('microphone', microphone, 'test-retry'), 'granted');
let release;
let calls = 0;
const queue = permissionRefreshQueue(async () => { calls++; if (calls === 1) await new Promise(resolve => { release = resolve; }); });
const first = queue.run();
const second = queue.run();
assert.equal(first, second, 'concurrent calls share one in-flight check');
assert.equal(calls, 1);
release(); await first;
assert.equal(calls, 2, 'focus during a check replays after its result');
queue.dispose(); await queue.run(); assert.equal(calls, 2);
const onboarding = read('components/Onboarding.tsx');
const desktop = onboarding.slice(onboarding.indexOf('function DesktopOnboarding('), onboarding.indexOf('function OnboardingLoading('));
assert.equal((desktop.match(/<PermissionStep\b/g) ?? []).length, 3);
for (const status of ['accessibility', 'microphone', 'screenCapture']) {
  assert.ok(desktop.includes(`status={${status}}`));
  assert.ok(desktop.includes(`permissionAction(${status}) === 'retry' ? refresh`));
}
assert.ok(desktop.includes("t('onboarding.skipScreenPermission')"));
assert.ok(desktop.includes('onClick={onComplete}'));
const refresh = desktop.slice(desktop.indexOf('const refresh ='), desktop.indexOf('const request ='));
assert.ok(!refresh.includes('requestScreenCapturePermission('));
assert.ok(refresh.includes('dictationPermissionsReady(a, m) && permissionGranted(s)'));
assert.ok(refresh.includes('active()'));
const settings = read('pages/settings/PermissionsSection.tsx');
for (const status of ['accessibility', 'microphone', 'screenCapture']) assert.ok(settings.includes(`<PermissionPill status={${status}} />`));
for (const source of [desktop, settings]) assert.ok(!source.includes('resetAccessibilityPermissionAndRestartApp'));
assert.ok(!read('components/FloatingShell.tsx').includes('ScreenCapturePermissionBanner'));
assert.ok(!read('pages/settings/DataStorageSection.tsx').includes('requestScreenCapturePermission'));
assert.ok(!existsSync(fileURLToPath(new URL('../src/components/ScreenCapturePermissionBanner.tsx', import.meta.url))));
const lifecycle = read('lib/usePermissionRefresh.ts');
for (const event of ['focus', 'visibilitychange', 'onFocusChanged']) assert.ok(lifecycle.includes(event));
assert.ok(!lifecycle.includes('.show('));
assert.ok(!lifecycle.includes('requestScreenCapturePermission'));
const backend = readFileSync(new URL('../src-tauri/src/commands/permissions_cmds.rs', import.meta.url), 'utf8');
assert.ok(!backend.includes('.unwrap_or(PermissionStatus::NotApplicable)'));
console.log('PASS: independent failure/denial, retry-only actions, foreground grant refresh, optional skip, noDevice/unsupported/unavailable, serialized refresh, request isolation');
