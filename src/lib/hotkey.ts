import type { ComboBinding, ShortcutBinding } from './types';

export function defaultDictationHotkey(): ShortcutBinding {
  return { primary: 'RightCommand', modifiers: [] };
}

/** 把 ComboBinding 或 QaHotkeyBinding 格式化为可读标签，如 "⌘⇧D" / "Ctrl+Shift+D"。 */
/** 组合键的逐键显示段（修饰键按固定顺序 + 主键），供键帽（Kbd）逐键渲染。 */
export function formatComboParts(
  binding: ComboBinding | ShortcutBinding,
): string[] {
  const parts: string[] = [];
  const platform = currentPlatform();

  // 固定输出顺序：Ctrl/Cmd → Alt/Option → Shift → Super
  const modifierOrder = [
    'cmd-left', 'cmd-right', 'cmd', 'ctrl-left', 'ctrl-right', 'ctrl',
    'alt-left', 'alt-right', 'alt', 'shift-left', 'shift-right', 'shift',
    'super-left', 'super-right', 'super',
  ] as const;
  for (const tag of modifierOrder) {
    if (binding.modifiers.some(m => m.toLowerCase() === tag)) {
      parts.push(sideModifierDisplayName(tag, platform));
    }
  }

  parts.push(formatPrimary(binding.primary));
  return parts;
}

export function formatComboLabel(binding: ComboBinding | ShortcutBinding): string {
  return formatComboParts(binding).join(currentPlatform().isMac ? '' : '+');
}

export function currentPlatform(): { isMac: boolean; isWindows: boolean } {
  const nav = typeof navigator === 'undefined' ? null : navigator;
  const platform = nav?.platform || '';
  const userAgent = nav?.userAgent || '';
  return {
    isMac: platform.includes('Mac') || userAgent.includes('Mac'),
    isWindows: platform.includes('Win') || userAgent.includes('Windows'),
  };
}

/** Build side-specific modifier tags from Web KeyboardEvent.code values. */
export function sideModifiersFromPressedCodes(codes: Iterable<string>): string[] {
  const set = codes instanceof Set ? codes : new Set(codes);
  const modifiers: string[] = [];
  if (set.has('MetaLeft')) modifiers.push('cmd-left');
  else if (set.has('MetaRight')) modifiers.push('cmd-right');
  if (set.has('ControlLeft')) modifiers.push('ctrl-left');
  else if (set.has('ControlRight')) modifiers.push('ctrl-right');
  if (set.has('AltLeft')) modifiers.push('alt-left');
  else if (set.has('AltRight')) modifiers.push('alt-right');
  if (set.has('ShiftLeft')) modifiers.push('shift-left');
  else if (set.has('ShiftRight')) modifiers.push('shift-right');
  return modifiers;
}

/** Build generic modifier tags (cmd/super/ctrl/alt/shift) from pressed key codes. */
export function genericModifiersFromPressedCodes(codes: Iterable<string>): string[] {
  const set = codes instanceof Set ? codes : new Set(codes);
  const modifiers: string[] = [];
  const { isMac } = currentPlatform();
  if (isMac) {
    if (set.has('MetaLeft') || set.has('MetaRight')) modifiers.push('cmd');
  } else if (
    set.has('MetaLeft')
    || set.has('MetaRight')
    || set.has('OSLeft')
    || set.has('OSRight')
  ) {
    modifiers.push('super');
  }
  if (set.has('ControlLeft') || set.has('ControlRight')) modifiers.push('ctrl');
  if (set.has('AltLeft') || set.has('AltRight')) modifiers.push('alt');
  if (set.has('ShiftLeft') || set.has('ShiftRight')) modifiers.push('shift');
  return modifiers;
}

export function modifiersFromPressedCodes(
  codes: Iterable<string>,
  sideSpecific = false,
): string[] {
  return sideSpecific
    ? sideModifiersFromPressedCodes(codes)
    : genericModifiersFromPressedCodes(codes);
}

function sideModifierDisplayName(tag: string, platform: { isMac: boolean; isWindows: boolean }): string {
  const sideLabels: Record<string, string> = platform.isMac
    ? {
        'cmd-left': '左 ⌘',
        'cmd-right': '右 ⌘',
        'ctrl-left': '左 ⌃',
        'ctrl-right': '右 ⌃',
        'alt-left': '左 ⌥',
        'alt-right': '右 ⌥',
        'shift-left': '左 ⇧',
        'shift-right': '右 ⇧',
      }
    : {
        'cmd-left': platform.isWindows ? '左 Win' : '左 Super',
        'cmd-right': platform.isWindows ? '右 Win' : '右 Super',
        'ctrl-left': '左 Ctrl',
        'ctrl-right': '右 Ctrl',
        'alt-left': '左 Alt',
        'alt-right': '右 Alt',
        'shift-left': '左 Shift',
        'shift-right': '右 Shift',
        'super-left': platform.isWindows ? '左 Win' : '左 Super',
        'super-right': platform.isWindows ? '右 Win' : '右 Super',
      };
  if (sideLabels[tag]) return sideLabels[tag];
  if (platform.isMac) {
    switch (tag) {
      case 'cmd': return '\u2318';
      case 'ctrl': return '\u2303';
      case 'alt': return '\u2325';
      case 'shift': return '\u21E7';
      case 'super': return '\u2318';
    }
  } else {
    switch (tag) {
      case 'cmd': return platform.isWindows ? 'Ctrl' : 'Super';
      case 'ctrl': return 'Ctrl';
      case 'alt': return 'Alt';
      case 'shift': return 'Shift';
      case 'super': return platform.isWindows ? 'Win' : 'Super';
    }
  }
  return tag;
}

function formatPrimary(primary: string): string {
  const trimmed = primary.trim();
  if (!trimmed) return '?';
  // 单字母归大写
  if (trimmed.length === 1 && /[a-zA-Z]/.test(trimmed)) {
    return trimmed.toUpperCase();
  }
  // 常见命名键的 macOS 符号
  const isMac = currentPlatform().isMac;
  if (isMac) {
    switch (trimmed.toLowerCase()) {
      case 'space': return '\u2423';
      case 'enter':
      case 'return': return '\u21A9';
      case 'tab': return '\u21E5';
      case 'escape':
      case 'esc': return '\u238B';
      case 'backspace': return '\u232B';
      case 'delete':
      case 'del': return '\u2326';
      case 'arrowup':
      case 'up': return '\u2191';
      case 'arrowdown':
      case 'down': return '\u2193';
      case 'arrowleft':
      case 'left': return '\u2190';
      case 'arrowright':
      case 'right': return '\u2192';
    }
  }
  switch (trimmed.toLowerCase()) {
    case 'rightoption': return isMac ? 'Right ⌥' : 'Right Alt';
    case 'leftoption': return isMac ? 'Left ⌥' : 'Left Alt';
    case 'rightcontrol': return isMac ? 'Right ⌃' : 'Right Ctrl';
    case 'leftcontrol': return isMac ? 'Left ⌃' : 'Left Ctrl';
    case 'rightcommand': return isMac ? 'Right ⌘' : (currentPlatform().isWindows ? 'Right Win' : 'Right Super');
    case 'leftcommand': return isMac ? 'Left ⌘' : (currentPlatform().isWindows ? 'Left Win' : 'Left Super');
    case 'leftshift': return isMac ? 'Left ⇧' : 'Left Shift';
    case 'rightshift': return isMac ? 'Right ⇧' : 'Right Shift';
    case 'fn': return 'Fn';
    case 'mediaplaypause': return '⏯ Media';
    case 'shift': return isMac ? '⇧' : 'Shift';
  }
  return trimmed;
}
