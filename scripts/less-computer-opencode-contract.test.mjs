import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = fileURLToPath(new URL('..', import.meta.url));
const read = (relativePath) => readFile(join(appRoot, relativePath), 'utf8');

const [lib, miscCommands, coreAdapters] = await Promise.all([
  read("src-tauri/src/lib.rs"),
  read("src-tauri/src/commands/misc.rs"),
  read("src-tauri/src/core_adapters.rs"),
]);
const microphoneMenuStart = lib.indexOf('fn build_microphone_tray_menu');
const microphoneMenuEnd = lib.indexOf('pub(crate) fn refresh_tray_microphone_menu');
const microphoneMenu = lib.slice(microphoneMenuStart, microphoneMenuEnd);
assert(
  microphoneMenu.includes('TrayMicrophoneDeviceCache'),
  'tray menu construction must read the asynchronous microphone-device cache',
);
assert(
  !microphoneMenu.includes('recorder::list_input_devices'),
  'tray menu construction must never enumerate CoreAudio devices on the AppKit main thread',
);
assert(
  lib.includes('.name("openless-tray-mic-event".into())'),
  'native microphone notifications must dispatch enumeration to a background thread',
);
assert(
  miscCommands.includes('pub async fn list_microphone_devices(') &&
    miscCommands.includes('.platform') &&
    miscCommands.includes('.microphone_devices()') &&
    coreAdapters.includes('tauri::async_runtime::spawn_blocking(crate::recorder::list_input_devices'),
  'settings microphone enumeration must not block the AppKit main thread',
);
console.log("microphone host async contract passed");
