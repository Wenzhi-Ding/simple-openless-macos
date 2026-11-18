// 通用 → 录音与输入：录音快捷键 / 麦克风 / 胶囊 / 静音，外加「启动」折叠组。
// 「插入与剪贴板」组已随流式输入功能删除：上屏固定走一次性粘贴。

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShortcutRecorder } from '../../components/ShortcutRecorder';
import { defaultDictationHotkey } from '../../lib/hotkey';
import {
  isTauri,
  listMicrophoneDevices,
  playCue,
  setDictationHotkey,
} from '../../lib/ipc';
import { getPlatformCapabilities } from '../../lib/platform';
import type {
  CapsuleStyle,
  MicrophoneDevice,
  PlatformCapabilities,
} from '../../lib/types';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { SelectLite } from '../../components/ui/SelectLite';
import { Card, Collapsible } from '../_atoms';
import { SectionTitle, SettingRow, Toggle } from './shared';
import { MicrophoneSelect } from './MicrophoneSelect';

/** 「静音后自动停止」的可选静音时长（秒），见 issue #860。 */
const silenceAutoStopOptions = [1, 1.5, 2, 3, 4, 5];

async function autostartIsEnabled(): Promise<boolean> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<boolean>('plugin:autostart|is_enabled');
}

async function autostartEnable(): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('plugin:autostart|enable');
}

async function autostartDisable(): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('plugin:autostart|disable');
}

export function RecordingInputSection() {
  const { t } = useTranslation();
  const { prefs, capability, updatePrefs: savePrefs } = useHotkeySettings();
  const [platformCaps, setPlatformCaps] = useState<PlatformCapabilities | null>(null);
  const [microphoneDevices, setMicrophoneDevices] = useState<MicrophoneDevice[]>([]);
  const [microphoneDevicesLoaded, setMicrophoneDevicesLoaded] = useState(false);
  const [microphoneDevicesError, setMicrophoneDevicesError] = useState<string | null>(null);

  useEffect(() => {
    void getPlatformCapabilities().then(setPlatformCaps);
  }, []);

  const loadMicrophoneDevices = useCallback(async (
    signal?: { cancelled: boolean },
    options: { showLoading?: boolean } = {},
  ) => {
    if (options.showLoading ?? true) {
      setMicrophoneDevicesLoaded(false);
    }
    setMicrophoneDevicesError(null);
    try {
      const devices = await listMicrophoneDevices();
      if (signal?.cancelled) return;
      setMicrophoneDevices(devices);
      setMicrophoneDevicesLoaded(true);
    } catch (err) {
      console.error('[settings] list microphone devices failed', err);
      if (signal?.cancelled) return;
      setMicrophoneDevices([]);
      setMicrophoneDevicesError(err instanceof Error ? err.message : String(err));
      setMicrophoneDevicesLoaded(true);
    }
  }, []);

  useEffect(() => {
    const signal = { cancelled: false };
    void loadMicrophoneDevices(signal);
    return () => {
      signal.cancelled = true;
    };
  }, [loadMicrophoneDevices]);

  useEffect(() => {
    if (!isTauri) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    async function listenForDeviceChanges() {
      const { listen } = await import('@tauri-apps/api/event');
      if (cancelled) return;
      const stopListening = await listen('microphone:devices-changed', () => {
        void loadMicrophoneDevices(undefined, { showLoading: false });
      });
      if (cancelled) {
        stopListening();
        return;
      }
      unlisten = stopListening;
    }
    void listenForDeviceChanges();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadMicrophoneDevices]);

  if (!prefs || !capability) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

  const showDesktopHotkey = platformCaps?.supportsDesktopHotkey === true;
  const showDesktopStartup = showDesktopHotkey;

  const onShowCapsuleChange = (showCapsule: boolean) =>
    savePrefs({ ...prefs, showCapsule });
  const onMuteDuringRecordingChange = (muteDuringRecording: boolean) =>
    savePrefs({ ...prefs, muteDuringRecording });
  const onAudioCueChange = (audioCueOnRecord: boolean) =>
    savePrefs({ ...prefs, audioCueOnRecord });
  const onMicrophoneDeviceChange = (microphoneDeviceName: string) =>
    savePrefs({ ...prefs, microphoneDeviceName });
  const onStartMinimizedChange = (startMinimized: boolean) =>
    savePrefs({ ...prefs, startMinimized });

  const preferredMicrophoneAvailable = Boolean(
    prefs.microphoneDeviceName
    && microphoneDevices.some(device => device.name === prefs.microphoneDeviceName),
  );
  const effectiveMicrophoneDeviceName = prefs.microphoneDeviceName
    && (!microphoneDevicesLoaded || preferredMicrophoneAvailable)
    ? prefs.microphoneDeviceName
    : '';

  return (
    <>
      <Card>
        <SectionTitle hint={t('settings.recording.desc')}>
          {t('settings.recording.title')}
        </SectionTitle>
        {showDesktopHotkey && (
        <SettingRow label={t('settings.recording.hotkeyLabel')}>
          <ShortcutRecorder
            value={prefs.dictationHotkey}
            sideSpecificModifiers
            // 录音快捷键是核心热键，Rust 端不接受 null，不可停用——置灰并提示。
            disableDisabled
            disableHint={t('settings.recording.comboDisableHint')}
            onSave={async binding => {
              await setDictationHotkey(binding);
              await savePrefs({ ...prefs, dictationHotkey: binding });
            }}
            onReset={async () => {
              const binding = defaultDictationHotkey();
              await setDictationHotkey(binding);
              await savePrefs({ ...prefs, dictationHotkey: binding });
            }}
          />
        </SettingRow>
        )}
        {/* 「静音后自动停止」：开启后在本行下方展开「静音时长」选择行，走 grid
            0fr→1fr 过渡（与开关行动画同款）。终点高度固定（SettingRow 自身高度），
            反复开关不会跳动；inert 把折叠态控件移出 tab 顺序与 a11y 树。 */}
        {showDesktopHotkey && (
        <>
        <SettingRow
          label={t('settings.recording.silenceAutoStopLabel')}
          desc={t('settings.recording.silenceAutoStopDesc')}
        >
          {/* 开关行只放 Toggle：秒数下拉移出本行，避免开关时行内内容变宽导致抽搐。 */}
          <Toggle
            on={prefs.silenceAutoStopEnabled}
            onToggle={next => savePrefs({ ...prefs, silenceAutoStopEnabled: next })}
          />
        </SettingRow>
        <div
          style={{
            display: 'grid',
            gridTemplateRows: prefs.silenceAutoStopEnabled ? '1fr' : '0fr',
            transition:
              'grid-template-rows 0.22s var(--ol-motion-soft), opacity 0.18s var(--ol-motion-quick)',
            opacity: prefs.silenceAutoStopEnabled ? 1 : 0,
          }}
          {...(!prefs.silenceAutoStopEnabled ? { inert: '' } : {})}
          aria-hidden={!prefs.silenceAutoStopEnabled}
        >
          <div style={{ overflow: 'hidden', minHeight: 0 }}>
            <SettingRow label={t('settings.recording.silenceAutoStopSecondsLabel')}>
              {/* 与麦克风下拉同款视觉：SelectLite 默认触发器底色 + 固定 200 宽 →
                  右边缘与「MacBook Air 麦克风」对齐（control 区域同起点同宽）。 */}
              <SelectLite
                value={String(
                  silenceAutoStopOptions.includes(prefs.silenceAutoStopSeconds)
                    ? prefs.silenceAutoStopSeconds
                    : 3,
                )}
                onChange={next => savePrefs({ ...prefs, silenceAutoStopSeconds: Number(next) })}
                options={silenceAutoStopOptions.map(value => ({
                  value: String(value),
                  label: t('settings.recording.silenceAutoStopSecondsValue', { value }),
                }))}
                ariaLabel={t('settings.recording.silenceAutoStopSecondsLabel')}
                style={{ width: 200, maxWidth: '100%', minWidth: 0 }}
              />
            </SettingRow>
          </div>
        </div>
        </>
        )}
        <SettingRow label={t('settings.recording.microphoneLabel')} desc={t('settings.recording.microphoneDesc')}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            <MicrophoneSelect
              devices={microphoneDevices}
              selectedName={effectiveMicrophoneDeviceName}
              onSelect={onMicrophoneDeviceChange}
              onOpen={() => { void loadMicrophoneDevices(undefined, { showLoading: false }); }}
            />
            {microphoneDevicesError && (
              <div style={{ fontSize: 11, color: 'var(--ol-err)', lineHeight: 1.5 }}>
                {t('settings.recording.microphoneLoadError', { message: microphoneDevicesError })}
              </div>
            )}
          </div>
        </SettingRow>
        <SettingRow label={t('settings.recording.capsuleLabel')} desc={t('settings.recording.capsuleDesc')}>
          <Toggle on={prefs.showCapsule} onToggle={onShowCapsuleChange} />
        </SettingRow>
        <SettingRow label={t('settings.recording.capsuleStyleLabel')}>
          <SelectLite
            value={prefs.capsuleStyle ?? 'siri'}
            onChange={next => savePrefs({ ...prefs, capsuleStyle: next as CapsuleStyle })}
            options={[
              { value: 'siri', label: t('settings.recording.capsuleStyleSiri') },
              { value: 'classic', label: t('settings.recording.capsuleStyleClassic') },
            ]}
            ariaLabel={t('settings.recording.capsuleStyleLabel')}
            // 与麦克风/语言等下拉同款视觉：走 SelectLite 默认触发器底色
            //（--ol-select-trigger-bg），而不是 inputStyle 的 surface-2；宽度
            // 220 与同区粘贴快捷键行、语言区保持一致（minWidth 200 防缩窄）。
            style={{ maxWidth: 220, minWidth: 200 }}
          />
        </SettingRow>
        <SettingRow label={t('settings.recording.muteDuringRecordingLabel')} desc={t('settings.recording.muteDuringRecordingDesc')}>
          <Toggle on={prefs.muteDuringRecording} onToggle={onMuteDuringRecordingChange} />
        </SettingRow>
          <SettingRow
            label={t('settings.recording.audioCueLabel')}
            desc={t('settings.recording.audioCueDesc')}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <Toggle on={prefs.audioCueOnRecord} onToggle={onAudioCueChange} />
              <button
                type="button"
                onClick={() => playCue('start').catch(() => undefined)}
                style={{
                  padding: '5px 12px',
                  fontSize: 12,
                  fontWeight: 500,
                  fontFamily: 'inherit',
                  border: '0.5px solid var(--ol-line-strong)',
                  borderRadius: 8,
                  background: 'var(--ol-surface-2)',
                  color: 'var(--ol-ink-2)',
                  cursor: 'default',
                  transition: 'background 0.16s var(--ol-motion-quick)',
                }}
              >
                {t('settings.recording.audioCuePreview')}
              </button>
            </div>
          </SettingRow>
      </Card>

      {/* 「插入与剪贴板」组已随流式输入删除：上屏固定走一次性粘贴。 */}
      {/* ─── 启动（折叠） ──────────────────────────────────────────── */}
      {showDesktopStartup && (
      <Collapsible title={t('settings.recording.startupGroupTitle')}>
        <AutostartRow />
        <SettingRow label={t('settings.recording.startMinimizedLabel')}>
          <Toggle on={prefs.startMinimized} onToggle={onStartMinimizedChange} />
        </SettingRow>
        {/* 自动检查更新已随纯本地化删除（后端强制 autoUpdateCheck=false）。 */}
      </Collapsible>
      )}
    </>
  );
}

// 不存进 prefs：autostart 状态由 macOS LaunchAgent 持有，prefs 缓存反而会与 OS 真相不一致。
// 切换写入期间禁用开关（aria-busy）防连点乱序；写完回读系统实际状态作为
// 展示值 —— 展示永远跟随 OS 真相，而不是乐观值。
function AutostartRow() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  // 切 plist / 注册表失败时给用户看的错误。null = 没有失败/上次操作已成功。
  const [error, setError] = useState<string | null>(null);

  const readSystemState = useCallback(async () => {
    try {
      return await autostartIsEnabled();
    } catch (err) {
      console.error('[autostart] isEnabled failed', err);
      return null;
    }
  }, []);

  useEffect(() => {
    if (!isTauri) {
      setLoaded(true);
      return;
    }
    let cancelled = false;
    void readSystemState().then(value => {
      if (cancelled || value === null) return;
      setEnabled(value);
    }).finally(() => {
      if (!cancelled) setLoaded(true);
    });
    return () => {
      cancelled = true;
    };
  }, [readSystemState]);

  const onToggle = async (next: boolean) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setEnabled(next); // 乐观显示；下面立刻用系统回读校准
    try {
      if (isTauri) {
        if (next) await autostartEnable();
        else await autostartDisable();
      }
      const actual = await readSystemState();
      if (actual !== null) setEnabled(actual);
    } catch (err) {
      console.error('[autostart] toggle failed', err);
      setError(err instanceof Error ? err.message : String(err));
      const actual = await readSystemState();
      if (actual !== null) setEnabled(actual);
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingRow label={t('settings.recording.startupAtBoot')}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 4, alignItems: 'flex-start' }}>
        <Toggle
          on={enabled}
          onToggle={next => void onToggle(next)}
          disabled={!loaded}
          busy={busy}
          ariaLabel={t('settings.recording.startupAtBoot')}
        />
        {error && (
          <div style={{ fontSize: 11, color: 'var(--ol-err)', marginTop: 4, lineHeight: 1.5 }}>
            {t('settings.recording.startupAtBootError', { message: error })}
          </div>
        )}
      </div>
    </SettingRow>
  );
}
