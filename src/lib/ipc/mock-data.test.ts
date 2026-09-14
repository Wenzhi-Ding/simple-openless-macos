import { mockSettings, mockHotkeyCapability } from './mock-data';

if (mockSettings.autoUpdateCheck !== false) throw new Error('mock settings must disable update checks');
if (!mockSettings.cursorContextEnabled || !mockSettings.screenOcrContextEnabled) throw new Error('mock settings must enable fixed macOS context features');
if (mockHotkeyCapability.adapter !== 'macEventTap') throw new Error('mock hotkey must represent macOS');
console.log('mock-data.test.ts passed');
