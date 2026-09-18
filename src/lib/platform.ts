import type { PlatformCapabilities, PlatformKind } from './types';
export type { PlatformCapabilities, PlatformKind };
let cachedCapabilities: PlatformCapabilities | null = null;

/** Screen recording is a macOS-only permission in the current product. */
export function requiresScreenCapturePermission(capabilities: PlatformCapabilities): boolean {
  return capabilities.platform === 'macos';
}
export function inferPlatformCapabilities(): PlatformCapabilities {
  return { platform: 'macos', supportsDesktopHotkey: true, supportsTray: true, supportsOverlay: true, supportsImeInput: false, supportsLocalAsr: true, supportsLocalQwen3Mlx: true, supportsInAppDictation: false, supportsAutoUpdate: false };
}
export async function getPlatformCapabilities(): Promise<PlatformCapabilities> {
  if (cachedCapabilities) return cachedCapabilities;
  const isTauri = globalThis.window !== undefined && '__TAURI_INTERNALS__' in globalThis.window;
  if (!isTauri) return cachedCapabilities = inferPlatformCapabilities();
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    return cachedCapabilities = await invoke<PlatformCapabilities>('get_platform_capabilities');
  } catch (err) { console.warn('[platform] capability query unavailable; using desktop defaults', err); return cachedCapabilities = inferPlatformCapabilities(); }
}
export function getCachedPlatformCapabilities(): PlatformCapabilities | null { return cachedCapabilities; }
