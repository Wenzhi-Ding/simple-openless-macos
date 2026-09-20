// Icon.tsx — minimal stroke icons (1.5 stroke). Matches the black/blue aesthetic.
// Usage: <Icon name="overview" size={16} />

import type { CSSProperties } from 'react';

export const ICONS: Record<string, string> = {
  overview: 'M3 3v18h18M18 17V9M13 17V5M8 17v-3', // Simplified bar chart
  history: 'M12 8v4l3 3m6-3a9 9 0 1 1-18 0 9 9 0 0 1 18 0z', // Clock
  vocab: 'M4 19.5A2.5 2.5 0 0 1 6.5 17H20M4 19.5A2.5 2.5 0 0 0 6.5 22H20V4H6.5A2.5 2.5 0 0 0 4 6.5v13Z', // Book
  style: 'M12 2v20M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6', // Type / Font
  settings: 'M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0z', // Settings gear
  help: 'M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3M12 17h.01M12 22c5.523 0 10-4.477 10-10S17.523 2 12 2 2 6.477 2 12s4.477 10 10 10z', // Help circle
  mic: 'M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v4M8 23h8', // Mic
  search: 'M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.35-4.35', // Search
  plus: 'M12 5v14M5 12h14', // Plus
  check: 'M20 6L9 17l-5-5', // Check
  copy: 'M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2M15 2H9a1 1 0 0 0-1 1v2a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V3a1 1 0 0 0-1-1z', // Clipboard
  eye: 'M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7zM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z', // Eye
  trash: 'M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M10 11v6M14 11v6', // Trash
  refresh: 'M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5', // Refresh ccw
  sparkle: 'M12 3L14.5 9.5 21 12 14.5 14.5 12 21 9.5 14.5 3 12 9.5 9.5 12 3Z', // Sparkle
  bolt: 'M13 2L3 14h9l-1 8 10-12h-9l1-8z', // Zap
  clock: 'M12 8v4l3 3M12 22a10 10 0 1 1 0-20 10 10 0 0 1 0 20z', // Clock standard
  hash: 'M4 9h16M4 15h16M10 3L8 21M16 3l-2 18', // Hash
  chevDown: 'M6 9l6 6 6-6', // Chevron down
  chevRight: 'M9 18l6-6-6-6', // Chevron right
  chevLeft: 'M15 18l-6-6 6-6', // Chevron left
  cmd: 'M18 3a3 3 0 0 0-3 3v12a3 3 0 0 0 3 3 3 3 0 0 0 3-3 3 3 0 0 0-3-3H6a3 3 0 0 0-3 3 3 3 0 0 0 3 3 3 3 0 0 0 3-3V6a3 3 0 0 0-3-3 3 3 0 0 0-3 3 3 3 0 0 0 3 3h12a3 3 0 0 0 3-3 3 3 0 0 0-3-3z', // Command
  option: 'M3 3h6l6 18h6M14 3h7', // Option
  esc: 'M10 6l-6 6 6 6M4 12h16', // Move left (escape substitute)
  enter: 'M9 10L4 15l5 5M20 4v7a4 4 0 0 1-4 4H4', // Corner down left
  inserted: 'M20 6L9 17l-5-5', // Check
  mac: 'M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6zm8 14v2m-4 0h8', // Monitor
  doc: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6', // File
  user: 'M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z', // User
  shield: 'M12 22s8-3 8-9V5l-8-3-8 3v8c0 6 8 9 8 9z', // Shield (privacy)
  close: 'M18 6L6 18M6 6l12 12', // Close / X
  play: 'M5 3l14 9-14 9V3z', // Play
  download: 'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3', // Download
};

export interface IconProps {
  name: string;
  size?: number;
  stroke?: string;
  strokeWidth?: number;
  fill?: string;
  style?: CSSProperties;
  className?: string;
}

export function Icon({
  name,
  size = 16,
  stroke = 'currentColor',
  strokeWidth = 1.8,
  fill = 'none',
  style,
  className,
}: IconProps) {
  const d = ICONS[name];
  if (!d) return null;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={fill}
      stroke={stroke}
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={style}
      className={className}
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  );
}
