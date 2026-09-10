import {
  getCapsuleHostMetrics,
  getCapsuleMessageLayout,
  getCapsulePillMetrics,
} from './capsuleLayout.ts';

function assertEqual<T>(actual: T, expected: T, name: string) {
  if (actual !== expected) {
    throw new Error(`${name}: expected ${expected}, got ${actual}`);
  }
}

const metrics = getCapsulePillMetrics('mac');
assertEqual(metrics.width, 460, 'mac voice orb stage uses the demo-scale width');
assertEqual(metrics.height, 180, 'mac voice orb stage uses the demo-scale height');
assertEqual(metrics.textWidth, 400, 'mac voice orb text stays inside the stage');
assertEqual(metrics.boxSizing, 'border-box', 'mac voice orb stage keeps border-box sizing');

const host = getCapsuleHostMetrics('mac');
assertEqual(host.width, 460, 'mac voice orb host matches stage width');
assertEqual(host.height, 180, 'mac voice orb host matches stage height');
assertEqual(host.horizontalInset, 0, 'mac voice orb host has no side inset');
assertEqual(host.bottomInset, 0, 'mac voice orb host has no bottom inset');
assertEqual(host.boxSizing, 'border-box', 'mac voice orb host keeps border-box sizing');

for (const kind of ['default', 'processing', 'error'] as const) {
  const layout = getCapsuleMessageLayout('mac', kind);
  assertEqual(layout.lineClamp, 1, `mac ${kind} message stays single-line`);
  assertEqual(layout.allowWrap, false, `mac ${kind} message stays nowrap`);
}
