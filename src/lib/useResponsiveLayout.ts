import { useEffect, useState } from 'react';

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
