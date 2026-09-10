import { useEffect, useState } from 'react';
import { useHotkeySettings } from '../state/HotkeySettingsContext';

export function shouldUseNarrowLayout(breakpoint = 720): boolean {
  return typeof window !== 'undefined' && window.innerWidth < breakpoint;
}

export function useNarrowLayout(breakpoint = 720): boolean {
  const [narrow, setNarrow] = useState(() => shouldUseNarrowLayout(breakpoint));

  useEffect(() => {
    const sync = () => setNarrow(shouldUseNarrowLayout(breakpoint));
    sync();
    window.addEventListener('resize', sync);
    window.addEventListener('orientationchange', sync);
    return () => {
      window.removeEventListener('resize', sync);
      window.removeEventListener('orientationchange', sync);
    };
  }, [breakpoint]);

  return narrow;
}

/** 易读布局：只表示用户偏好，不混入窄窗口基础布局。 */
export function useReadableLayout(): boolean {
  const { prefs } = useHotkeySettings();
  return prefs?.stackedRowLayout === true;
}

/** 排版堆叠：窄窗口 或 用户开启「易读布局」。用于 grid/flex 换行，不含底栏等壳层逻辑。 */
export function useLayoutStack(breakpoint = 720): boolean {
  const narrow = useNarrowLayout(breakpoint);
  const readable = useReadableLayout();
  return narrow || readable;
}

/** 保守排版：仅读用户偏好，不与 窄窗口联动。 */
export function useConservativeLayout(): boolean {
  const { prefs } = useHotkeySettings();
  return prefs?.conservativeLayout === true;
}
