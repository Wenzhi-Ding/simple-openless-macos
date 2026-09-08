import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), 'utf8');
const coordinator = await read('src-tauri/src/coordinator.rs');
const coordinatorDictation = await read('src-tauri/src/coordinator/dictation_core.rs');
const coordinatorCapsule = await read('src-tauri/src/coordinator/capsule_focus.rs');
const coordinatorHotkeys = await read('src-tauri/src/coordinator/hotkey_loops.rs');
const tauriCoordinatorHost = await read('src-tauri/src/tauri_coordinator_host.rs');
const coreAdapters = await read('src-tauri/src/core_adapters.rs');
const dictionaryCommand = await read('src-tauri/src/commands/dictionary.rs');
const stylePacksCommand = await read('src-tauri/src/commands/style_packs.rs');
const hostDocumentMacos = await read('src-tauri/src/host_document/macos.rs');
const historyCommand = await read('src-tauri/src/commands/history.rs');
const appShell = await read('src/App.tsx');
const localAsrCore = await read('crates/openless-core/src/local_asr_service.rs');
const credentialCore = await read('crates/openless-core/src/credentials.rs');
const androidNativeBridge = await read('src-tauri/src/android/native_bridge.rs');
const androidKotlinBridge = await read('android/kotlin/OpenLessNative.kt');
const androidOverlayService = await read('android/kotlin/OpenLessOverlayService.kt');
const mobileHotkey = await read('src-tauri/src/mobile_stubs/hotkey.rs');
const mobileSelection = await read('src-tauri/src/mobile_stubs/selection.rs');
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
for (const method of [
  'accept_pending_correction',
  'reject_pending_correction',
  'dismiss_pending_corrections',
]) {
  assert.match(
    dictionaryCommand,
    new RegExp(`core\\.${method}\\(`),
    `vocabulary suggestion command must delegate ${method} to Core`,
  );
}
assert.match(
  dictionaryCommand,
  /refresh_vocab_suggestion_presentation/,
  'the Tauri command may only pass Core suggestion presence to the host presentation seam',
);
assert.doesNotMatch(
  dictionaryCommand,
  /coord\.(?:accept_pending_correction|reject_pending_correction|dismiss_vocab_suggestions)\(/,
  'Tauri Coordinator must not own vocabulary suggestion mutations',
);
assert.match(
  stylePacksCommand,
  /core\.preview_style_pack_runtime\(&style_pack\)/,
  'style-pack runtime diagnostics must be assembled by Core',
);
const stylePackPreviewCommand = stylePacksCommand.match(
  /pub fn preview_style_pack_runtime\([^]*?\r?\n\}\r?\n/,
)?.[0];
assert.ok(stylePackPreviewCommand, 'style-pack preview command must remain present');
assert.doesNotMatch(
  stylePackPreviewCommand,
  /CoordinatorState/,
  'style-pack commands must not reach back into Coordinator for business diagnostics',
);
assert.doesNotMatch(
  coordinatorBusinessSources,
  /\bActiveAsr\b|CredentialsVault|ProviderScope|build_active_omni_provider|build_tauri_omni_provider|(?:fn|const)\s+(?:asr_vocab_phrases|prioritize_vocab_for_asr|FRESH_VOCAB_SEATS)\b/,
  'Coordinator must not own ASR provider, credential, active-session, or vocabulary policy',
);
assert.doesNotMatch(
  hostDocumentMacos,
  /\b(?:edit_is_within_typed_text|is_vocab_worthy|learned_rule)\b/,
  'the macOS edit watcher must report EditPair and leave learning policy to Core',
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
const localAsrActivation = localAsrCore.match(/    fn activate\([^]*?(?=\r?\n    fn )/)?.[0];
assert.ok(localAsrActivation, 'Core must own local ASR activation');
assert.match(localAsrActivation, /Self::validate_activation_provider\(&request\.target, &provider_type\)/,
  'activation must validate the native provider before preparation');
assert.match(localAsrActivation, /\.prepare\([^]*?\.preload_lease\([^]*?preferences\.update\(/,
  'activation must prepare and preload before committing preferences');
assert.match(localAsrActivation, /release_activation_lease\([^]*?\.await[^]*?restore_activation_runtime\([^]*?\.await/,
  'activation cleanup failures must await restoration before returning');
assert.match(localAsrActivation, /preferences\.get\(\)\.active_asr_provider[^]*?restore_activation_preferences[^]*?restore_activation_runtime/,
  'concurrent preference changes must roll back both preferences and native runtime');
assert.match(
  credentialCore,
  /ChannelMutation::ActivateLocalAsr \{ id, provider_type \} => \{[^]*?allocate_channel_id[^]*?channel\.enabled = true[^]*?channels\.insert\(0, channel\)[^]*?ChannelMutationResult::Activated\(id\)/,
  'Core metadata must create or enable and promote the activated channel in the same mutation',
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
assert.match(
  androidNativeBridge,
  /nativeBackendSnapshot[\s\S]*?android_backend_snapshot_response\(CORE_BACKEND\.get\(\)\.map\(Arc::as_ref\)\)/,
  'Android JNI must expose the typed Core startup handshake',
);
assert.match(
  androidKotlinBridge,
  /BACKEND_CONTRACT_VERSION = "2\.0\.0"[\s\S]*?requireBackendContract\(\)[\s\S]*?contractVersion/,
  'Android Kotlin must reject a backend contract version other than 2.0.0',
);
assert.match(
  androidOverlayService,
  /onCreate\(\)[\s\S]*?OpenLessNative\.requireBackendContract\(\)[\s\S]*?stopSelf\(\)/,
  'Android overlay startup must execute the JNI contract handshake before accepting actions',
);
assert.match(
  mobileHotkey,
  /pub fn next_press_id\(\) -> u64/,
  'the mobile hotkey stub must preserve the Core press-identity shape',
);
assert.match(
  mobileSelection,
  /capture_selection_insertion_target[^]*?reactivate_selection_insertion_target[^]*?true/,
  'mobile dictation insertion must provide a no-op target restore without enabling Selection Polish',
);
assert.doesNotMatch(coreAdapters, /\b(?:streamed_text|stream_failed)\b|starts_with\(/,
  'Host insertion adapter must not retain final reconciliation policy');
assert.match(coreAdapters, /fn preload_lease\([^]*?preload_for_lease\(\s*lease\.target,\s*model_dir,\s*provider_type,\s*Some\(lease\.generation\),?\s*\)/,
  'native preload must receive the Core activation generation');
console.log("retained shared host/history/style/activation contracts passed");
const providersCommand = await read("src-tauri/src/commands/providers.rs");
assert.match(
  providersCommand,
  /\.provider\s*\n?\s*\.validate\(/,
  'Tauri provider validation command must delegate to Core ProviderApi',
);
assert.match(
  providersCommand,
  /\.provider\s*\n?\s*\.list_models\(/,
  'Tauri provider model-list command must delegate to Core ProviderApi',
);
for (const forbiddenProviderBusinessToken of [
  /CredentialsVault/,
  /ProviderScope/,
  /ProviderConfig/,
  /reqwest::/,
  /validate_provider_service/,
  /list_provider_models_service/,
  /tokio::time::timeout/,
  /build_active_omni_provider/,
]) {
  assert.doesNotMatch(
    providersCommand,
    forbiddenProviderBusinessToken,
    'Tauri provider commands must not recreate Core credential/protocol business logic',
  );
}
const providerSection = await read("src/pages/settings/ProvidersSection.tsx");
const channelList = await read("src/pages/settings/ChannelList.tsx");
const providerLabels = await read("src/pages/settings/shared.tsx");
const providerUiSources = `${providerSection}\n${channelList}\n${providerLabels}`;
assert.match(
  providerUiSources,
  /listProviderDescriptors/,
  'React provider UI must consume Core ProviderDescriptor',
);
assert.doesNotMatch(
  providerUiSources,
  /\b(?:ASR_PRESETS|LLM_PRESETS|OMNI_PRESETS|LOCAL_ASR_PROVIDER_IDS)\b/,
  'React must retain labels only, not provider defaults or capability tables',
);
assert.doesNotMatch(
  providerUiSources,
  /https:\/\/(?:api\.openai\.com|api\.deepseek\.com|dashscope\.aliyuncs\.com|ark\.cn-beijing\.volces\.com)/,
  'React must not hard-code provider endpoints owned by Core descriptors',
);
console.log('shared-backend-wire-contract.test.mjs passed');
