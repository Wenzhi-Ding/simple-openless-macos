// Fixed local dictation: recording and app shortcuts only.
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShortcutRecorder } from '../../components/ShortcutRecorder';
import { defaultOpenAppShortcut, hotkeyModeSuffix } from '../../lib/hotkey';
import { setDictationHotkey, setOpenAppHotkey } from '../../lib/ipc';
import { getPlatformCapabilities } from '../../lib/platform';
import type { PlatformCapabilities } from '../../lib/types';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card } from '../_atoms';
import { SettingRow } from './shared';

export function ShortcutsSection() {
  const { t } = useTranslation();
  const { prefs, hotkey, updatePrefs: savePrefs } = useHotkeySettings();
  const [platformCaps, setPlatformCaps] = useState<PlatformCapabilities | null>(null);
  useEffect(() => { void getPlatformCapabilities().then(setPlatformCaps); }, []);
  if (!prefs || !hotkey) return <Card>{t('common.loading')}</Card>;
  if (platformCaps && !platformCaps.supportsDesktopHotkey) return null;
  return (
    <Card>
      <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 6 }}>{t('settings.shortcuts.title')}</div>
      <SettingRow label={t('settings.shortcuts.startStop')}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 6, width: '100%' }}>
          <ShortcutRecorder value={prefs.dictationHotkey} sideSpecificModifiers disableDisabled
            disableHint={t('settings.recording.comboDisableHint')}
            onSave={async binding => {
              await setDictationHotkey(binding);
              await savePrefs({ ...prefs, dictationHotkey: binding });
            }} />
          <div style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{hotkeyModeSuffix(hotkey.mode)}</div>
        </div>
      </SettingRow>
      <SettingRow label={t('settings.shortcuts.openApp')}>
        <ShortcutRecorder value={prefs.openAppHotkey}
          onSave={async binding => {
            await setOpenAppHotkey(binding);
            await savePrefs({ ...prefs, openAppHotkey: binding });
          }}
          onDisable={async () => {
            await setOpenAppHotkey(null);
            await savePrefs({ ...prefs, openAppHotkey: null });
          }}
          onReset={async () => {
            const binding = defaultOpenAppShortcut();
            await setOpenAppHotkey(binding);
            await savePrefs({ ...prefs, openAppHotkey: binding });
          }} />
      </SettingRow>
      <SettingRow label={t('settings.shortcuts.cancel')}><kbd>Esc</kbd></SettingRow>
    </Card>
  );
}
