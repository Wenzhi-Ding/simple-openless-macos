import {
  defaultDictationHotkey,
  formatComboLabel,
  genericModifiersFromPressedCodes,
  modifiersFromPressedCodes,
  sideModifiersFromPressedCodes,
} from './hotkey';

function assertEqual<T>(actual: T, expected: T, message: string) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function assertDeepEqual(actual: string[], expected: string[], message: string) {
  assertEqual(actual.join(','), expected.join(','), message);
}

function mockNavigator(platform: string, userAgent = '') {
  Object.defineProperty(globalThis, 'navigator', {
    value: { platform, userAgent },
    configurable: true,
  });
}

assertDeepEqual(
  sideModifiersFromPressedCodes(new Set(['MetaLeft', 'ShiftRight'])),
  ['cmd-left', 'shift-right'],
  'MetaLeft+ShiftRight produces side tags',
);

assertDeepEqual(
  modifiersFromPressedCodes(new Set(['MetaLeft', 'ShiftRight']), true),
  ['cmd-left', 'shift-right'],
  'side-specific mode uses side tags',
);

mockNavigator('MacIntel', 'Macintosh');
assertEqual(
  defaultDictationHotkey().primary,
  'RightCommand',
  'default dictation hotkey matches the macOS product default',
);
assertEqual(
  formatComboLabel(defaultDictationHotkey()),
  'Right ⌘',
  'formats the default macOS dictation hotkey',
);
assertDeepEqual(
  genericModifiersFromPressedCodes(new Set(['MetaLeft', 'ShiftRight'])),
  ['cmd', 'shift'],
  'macOS generic mode maps Meta to cmd',
);

mockNavigator('Win32', 'Windows');
assertDeepEqual(
  genericModifiersFromPressedCodes(new Set(['MetaLeft', 'ShiftRight'])),
  ['super', 'shift'],
  'non-mac generic mode maps Meta to super',
);

assertDeepEqual(
  modifiersFromPressedCodes(new Set(['MetaLeft', 'ShiftRight'])),
  ['super', 'shift'],
  'default recording on non-mac uses super',
);

assertDeepEqual(
  sideModifiersFromPressedCodes(new Set(['MetaRight', 'KeyD'])),
  ['cmd-right'],
  'MetaRight+D binding uses cmd-right only',
);

assertDeepEqual(
  sideModifiersFromPressedCodes(new Set(['MetaLeft', 'MetaRight'])),
  ['cmd-left'],
  'left cmd wins when both meta keys are tracked',
);

console.log('hotkeySideModifiers.test.ts passed');
