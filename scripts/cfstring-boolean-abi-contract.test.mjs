import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const root = new URL('../', import.meta.url);
const focusTarget = await readFile(new URL('src-tauri/src/focus_target.rs', root), 'utf8');
const host = await readFile(new URL('src-tauri/src/host_document/macos.rs', root), 'utf8');
const declaration = /fn CFStringGetCString\([\s\S]*?\) -> u8;/g;
assert.equal((host.match(declaration) ?? []).length, 1);
assert.doesNotMatch(host, /fn CFStringGetCString\([\s\S]*?\) -> bool;/);
assert.match(host, /if ok == 0 \{/);
assert.doesNotMatch(focusTarget, /CFStringGetCString|CFStringGetLength/);
console.log('CoreFoundation Boolean ABI declaration is u8; foreground target uses UTF8String');
