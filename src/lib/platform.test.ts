import { requiresScreenCapturePermission } from './platform';
import type { PlatformCapabilities } from './types';

const macos: PlatformCapabilities = {
  platform: 'macos', supportsDesktopHotkey: true, supportsTray: true, supportsOverlay: true,
  supportsImeInput: false, supportsLocalAsr: true, supportsLocalQwen3Mlx: true,
  supportsInAppDictation: false, supportsAutoUpdate: false,
};

if (!requiresScreenCapturePermission(macos)) {
  throw new Error('macos must display and check the screen-recording permission branch');
}
console.log('platform.test.ts passed');
