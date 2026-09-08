import { useEffect, useRef } from 'react';
import { isTauri } from './ipc/shared';
import { permissionRefreshQueue } from './permissionOnboarding';

// All entry points (focus, visibilitychange, onFocusChanged and buttons) use this queue.
// Checks never request access or show a window. Ignore results after unmount.
export function usePermissionRefresh(check: (active: () => boolean) => Promise<void>) {
  const latest = useRef(check);
  latest.current = check;
  const queue = useRef<ReturnType<typeof permissionRefreshQueue>>();
  useEffect(() => {
    let disposed = false;
    const current = permissionRefreshQueue(() => latest.current(() => !disposed));
    queue.current = current;
    const focus = () => { void current.run().catch(() => console.warn('[permissions] status=checkFailed source=refresh')); };
    const visible = () => { if (document.visibilityState === 'visible') focus(); };
    focus();
    window.addEventListener('focus', focus);
    document.addEventListener('visibilitychange', visible);
    let unlisten: (() => void) | undefined;
    if (isTauri) {
      void import('@tauri-apps/api/window')
        .then(({ getCurrentWindow }) => getCurrentWindow().onFocusChanged(({ payload }) => {
          if (payload) focus();
        }))
        .then(fn => { if (disposed) fn(); else unlisten = fn; })
        .catch(() => console.warn('[permissions] status=unavailable source=focusSubscription'));
    }
    return () => {
      disposed = true;
      current.dispose();
      window.removeEventListener('focus', focus);
      document.removeEventListener('visibilitychange', visible);
      unlisten?.();
    };
  }, []);
  return () => queue.current?.run() ?? Promise.resolve();
}
