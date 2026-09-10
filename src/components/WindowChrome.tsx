import { type CSSProperties, type ReactNode } from 'react';

export type OS = 'mac';

export function detectOS(): OS {
  return 'mac';
}

const MAC_TITLEBAR_HEIGHT = 28;
const MAC_SYSTEM_CONTROLS_RESERVED_WIDTH = 76;

interface WindowChromeProps {
  os?: OS;
  title?: string;
  children: ReactNode;
  height?: number | string;
}

export function WindowChrome({
  children,
  height = 800,
}: WindowChromeProps) {
  return (
    <div
      className="ol-winchrome"
      style={{
        '--ol-window-shell-radius': '0px',
        '--ol-window-console-radius': '20px',
        '--ol-window-titlebar-height': `${MAC_TITLEBAR_HEIGHT}px`,
        width: '100%',
        height,
        position: 'relative',
        borderRadius: 'var(--ol-window-shell-radius)',
        boxShadow: 'var(--ol-shadow-xl)',
        overflow: 'hidden',
        display: 'flex',
        flexDirection: 'column',
        border: 'none',
        background: 'var(--ol-window-bg)',
        backdropFilter: 'none',
        WebkitBackdropFilter: 'none',
        animation: 'ol-window-enter 0.42s var(--ol-motion-spring) both',
        transition: 'box-shadow 0.28s var(--ol-motion-soft), border-color 0.28s var(--ol-motion-soft)',
        willChange: 'opacity, transform',
      } as CSSProperties}
    >
      <div
        data-tauri-drag-region
        style={{
          position: 'absolute',
          top: 0,
          left: MAC_SYSTEM_CONTROLS_RESERVED_WIDTH,
          right: 0,
          height: MAC_TITLEBAR_HEIGHT,
          zIndex: 50,
        }}
      />
      <div style={{ flex: 1, minHeight: 0, display: 'flex', position: 'relative' }}>
        {children}
      </div>
    </div>
  );
}
