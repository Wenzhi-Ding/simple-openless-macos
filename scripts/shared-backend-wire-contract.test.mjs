import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), 'utf8');
const coordinator = await read('src-tauri/src/coordinator.rs');
const coordinatorDictation = await read('src-tauri/src/coordinator/dictation_core.rs');
const coordinatorCapsule = await read('src-tauri/src/coordinator/capsule_focus.rs');
const coordinatorHotkeys = await read('src-tauri/src/coordinator/hotkey_loops.rs');
const tauriCoordinatorHost = await read('src-tauri/src/tauri_coordinator_host.rs');
const coreAdapters = await read('src-tauri/src/core_adapters.rs');
const hostDocumentMacos = await read('src-tauri/src/host_document/macos.rs');
const hotwords = await read('src-tauri/src/hotwords.rs');
const tauriLib = await read('src-tauri/src/lib.rs');
const commandsModule = await read('src-tauri/src/commands/mod.rs');
const dictionaryCommand = await read('src-tauri/src/commands/dictionary.rs');
const capabilities = await read('src-tauri/capabilities/default.json');
const historyCommand = await read('src-tauri/src/commands/history.rs');
const appShell = await read('src/App.tsx');
const frontendIpcShared = await read('src/lib/ipc/shared.ts');
const coreLib = await read('crates/openless-core/src/lib.rs');
const localAsrCore = await read('crates/openless-core/src/local_asr_service.rs');
const localAsrDomain = await read('crates/openless-core/src/domains.rs');
const coordinatorBusinessSources = [
  coordinator,
  coordinatorDictation,
  coordinatorCapsule,
  coordinatorHotkeys,
].join('\n');
assert.doesNotMatch(
  coordinatorBusinessSources,
  /\.emit\(\s*["']local-asr-token["']/,
  'local-asr-token must be derived from typed Core events by the centralized Tauri bridge',
);
const frontendContractVersion = frontendIpcShared.match(/BACKEND_CONTRACT_VERSION\s*=\s*["']([^"']+)/)?.[1];
const coreContractVersion = coreLib.match(/BACKEND_CONTRACT_VERSION:\s*&str\s*=\s*["']([^"']+)/)?.[1];
assert.ok(frontendContractVersion && coreContractVersion, 'both frontend and Core contract versions must be declared');
assert.equal(frontendContractVersion, coreContractVersion, 'frontend startup handshake must match the Core backend contract version');
assert.match(
  coordinatorDictation,
  /\.dispatch_dictation_hotkey_edge\(edge\)/,
  'Tauri dictation hotkeys must delegate the complete session transition to Core',
);
assert.match(
  coordinatorDictation,
  /async fn dispatch\([^]*?Result<openless_core::CliDispatchOutcome,\s*openless_core::BackendError>/,
  'the hotkey adapter must retain the typed Core transition result',
);
assert.doesNotMatch(
  coordinatorBusinessSources,
  /inner\.translation_active|translation_active:\s*AtomicBool|fn finish_bookkeeping\(/,
  'translation intent must not survive in Host state or depend on one stop entry for cleanup',
);
const coordinatorInner = coordinator.match(/struct Inner \{([^]*?)\n\}/)?.[1];
assert.ok(coordinatorInner, 'Coordinator Inner must remain inspectable by the architecture contract');
assert.doesNotMatch(
  coordinatorInner,
  /AppHandle|AppHandleSlot|\bapp\s*:/,
  'Coordinator must not retain a Tauri AppHandle',
);
assert.match(
  coordinatorInner,
  /host: crate::tauri_coordinator_host::TauriCoordinatorHost/,
  'Coordinator must depend on the explicit Tauri host Module',
);
assert.doesNotMatch(
  coordinatorBusinessSources,
  /\.emit(?:_to)?\(/,
  'Coordinator modules must publish typed Core events or call semantic Tauri host actions',
);
assert.match(
  tauriCoordinatorHost,
  /struct TauriCoordinatorHost[^{]*\{[^]*?AppHandleSlot/,
  'the late-bound AppHandle must be owned by the Tauri host Module',
);
assert.doesNotMatch(
  tauriCoordinatorHost,
  /crate::coordinator::(?:Inner|capsule_focus)/,
  'the Tauri host must not reach back into Coordinator internals to operate capsule windows',
);
assert.doesNotMatch(
  coordinator,
  /use tauri::AppHandle|fn bind_app\s*\(/,
  'Coordinator must expose its Tauri host seam instead of accepting AppHandle directly',
);
assert.match(
  coordinator,
  /\nmod capsule_focus;/,
  'capsule_focus must remain private to the Coordinator module',
);
assert.doesNotMatch(
  coreAdapters,
  /managed_coordinator|try_state::<Arc<crate::coordinator::Coordinator>>/,
  'Core adapters must receive narrow shared host state instead of reaching back into Coordinator',
);
for (const legacyProviderCopy of [
  /#\[cfg\(any\(\)\)\]/,
  /\bTauriCloudTranscriptionEngine\b/,
  /\bTauriCloudTextPolisher\b/,
  /\bTauriOmniDictationEngine\b/,
  /\bbuild_tauri_omni_provider\b/,
]) {
  assert.doesNotMatch(
    coreAdapters,
    legacyProviderCopy,
    'Tauri must not retain a disabled copy of provider protocol construction owned by openless-core',
  );
}
assert.match(
  coordinatorInner,
  /hotkey_status: Arc<Mutex<HotkeyStatus>>/,
  'Coordinator and the platform adapter must share one hotkey status slot',
);
assert.doesNotMatch(
  coreAdapters,
  /SelectionRuntimeAdapter|TauriSelectionRuntime|TauriEditObservationAdapter|EditObservationAdapter/,
  'Tauri must not retain selection or edit-learning runtime adapters',
);
assert.doesNotMatch(
  coordinatorBusinessSources,
  /selection_polish|vocab_card_visible|pending_correction/,
  'Coordinator must not retain selection or vocabulary-suggestion presentation state',
);
assert.doesNotMatch(
  hostDocumentMacos,
  /\b(?:edit_is_within_typed_text|is_vocab_worthy|learned_rule)\b/,
  'the macOS host must not retain edit-learning policy',
);
assert.doesNotMatch(
  hotwords,
  /hotwords\.txt|OPENLESS_HOTWORDS|std::env::var|HOME|std::fs::read_to_string/,
  'hotword correction must use only the per-session frozen word list',
);
assert.match(
  hotwords,
  /pub fn correct\(text: &str, hotwords: &\[String\]\)/,
  'hotword correction must accept the session word list explicitly',
);
assert.doesNotMatch(
  tauriLib,
  /selection_polish|selection_polish_preview|accept_pending_correction|reject_pending_correction|dismiss_pending_corrections/,
  'the Tauri invoke handler must not expose removed selection or learning commands',
);
assert.doesNotMatch(
  commandsModule,
  /mod selection_polish/,
  'the command module must not restore selection polish commands',
);
assert.match(
  commandsModule,
  /mod dictionary;/,
  'the command module must retain the manual vocabulary IPC surface',
);
for (const command of ['list_vocab', 'add_vocab', 'remove_vocab', 'set_vocab_enabled']) {
  assert.match(
    dictionaryCommand,
    new RegExp(`pub fn ${command}\\b`),
    `manual vocabulary command ${command} must remain available`,
  );
}
assert.doesNotMatch(
  dictionaryCommand,
  /pending_correction|correction_rule|vocab_presets|suggestion/,
  'manual vocabulary commands must not restore learning, correction rules, or presets',
);
assert.doesNotMatch(
  capabilities,
  /selection-polish-preview/,
  'the capability must not grant access to a removed preview window',
);
assert.match(
  historyCommand,
  /core\.apply_history_retranscription\(/,
  'history retranscription mutation and attribution must be owned by Core',
);
assert.doesNotMatch(
  historyCommand,
  /entry\.(?:raw_transcript|final_text|asr_provider|asr_model|llm_provider|llm_model|polish_ms)\s*=/,
  'the Tauri history command must not mutate retranscription records',
);
assert.match(
  appShell,
  /export function App\([^]*?getStartupSnapshot\(\)[^]*?return <ReadyApp \{\.\.\.props\} \/>/,
  'every Tauri webview route must mount behind the shared readiness handshake',
);
assert.match(
  localAsrDomain,
  /pub trait LocalAsrApi[^]*?fn runtime_status[^]*?fn prepare[^]*?fn cancel_prepare[^]*?fn release/,
  'Core must own the fixed local ASR lifecycle contract',
);
assert.doesNotMatch(
  localAsrCore,
  /fn activate|validate_activation_provider|preload_lease|restore_activation_runtime/,
  'fixed Qwen local ASR must not retain selectable activation or rollback machinery',
);
assert.doesNotMatch(
  coreAdapters,
  /CredentialStore|InMemoryCredentialStore|credentials:changed/,
  'the local-only host must not retain credential storage or credential status events',
);
for (const legacyDictationFacade of [
  'start_dictation',
  'start_dictation_with_translation',
  'stop_dictation',
  'stop_dictation_with_translation',
  'cancel_dictation',
]) {
  assert.doesNotMatch(
    coordinator,
    new RegExp(`\\bpub(?:\\(crate\\))?\\s+(?:async\\s+)?fn\\s+${legacyDictationFacade}\\b`),
    `Coordinator must not expose the legacy ${legacyDictationFacade} facade; production entry points use openless-core`,
  );
}
assert.doesNotMatch(coreAdapters, /\b(?:streamed_text|stream_failed)\b|starts_with\(/,
  'Host insertion adapter must not retain final reconciliation policy');
assert.match(
  coreAdapters,
  /impl openless_core::ModelRuntimeAdapter for TauriLocalAsrRuntimeAdapter[^]*?fn prepare\([^]*?ensure_loaded_with_progress[^]*?fn preload\([^]*?status_snapshot[^]*?runtime_ready/,
  'native fixed-Qwen adapter must prepare and preload through the shared runtime',
);
console.log("retained shared host/history/activation contracts passed");
console.log('shared-backend-wire-contract.test.mjs passed');
