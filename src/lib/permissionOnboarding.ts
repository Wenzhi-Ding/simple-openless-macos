import type { PermissionStatus } from './types';

// UI failures are not OS denials; never feed them to a permission request.
export type PermissionCheck = PermissionStatus | 'loading' | 'checkFailed' | 'unavailable';
export const SCREEN_PERMISSION_REVIEWED_KEY = 'openless.screenPermissionReviewed';
export const permissionGranted = (status: PermissionCheck) =>
  status === 'granted' || status === 'notApplicable';
export const permissionAction = (status: PermissionCheck) =>
  permissionGranted(status) || status === 'loading' ? 'none'
    : status === 'checkFailed' || status === 'unavailable' || status === 'noDevice' ? 'retry' : 'request';
export const permissionStatusKey = (status: PermissionCheck) =>
  status === 'loading' ? 'checking' : status === 'checkFailed' || status === 'unavailable' ? 'checkFailed'
    : status === 'granted' ? 'granted' : status === 'notApplicable' ? 'notApplicable'
      : status === 'noDevice' ? 'micNoDevice' : 'denied';
export const dictationPermissionsReady = (accessibility: PermissionCheck, microphone: PermissionCheck) =>
  permissionGranted(accessibility) && (permissionGranted(microphone) || microphone === 'noDevice');
export const screenPermissionNeedsReview = (isMac: boolean, reviewed: boolean, status: PermissionCheck) =>
  isMac && !reviewed && !permissionGranted(status);

// Each result is published independently. Log only status and checking source, never IPC error payloads.
export async function checkPermission(name: string, check: () => Promise<PermissionStatus>, source: string): Promise<PermissionCheck> {
  let status: PermissionCheck;
  try { status = await check(); } catch { status = 'checkFailed'; }
  console.info(`[permissions] name=${name} status=${status} source=${source}`);
  return status;
}

// Coalesce simultaneous foreground/manual refreshes, but always replay a focus event
// arriving during an older check. No older overlapping result can overwrite the new one.
export function permissionRefreshQueue(check: () => Promise<void>) {
  let running: Promise<void> | undefined;
  let pending = false;
  let disposed = false;
  return {
    run(): Promise<void> {
      if (disposed) return Promise.resolve();
      pending = true;
      if (!running) running = (async () => {
        do { pending = false; await check(); } while (pending && !disposed);
      })().finally(() => { running = undefined; });
      return running;
    },
    dispose() { disposed = true; },
  };
}
