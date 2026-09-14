// 权限面板：麦克风 / 辅助功能 / 屏幕录制 / 全局热键。
// 内含三个状态 Pill + 适配器名称翻译辅助函数。

import { useEffect, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Icon } from '../../components/Icon';
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
  getHotkeyStatus,
  openSystemSettings,
  requestAccessibilityPermission,
  requestMicrophonePermission,
} from '../../lib/ipc';
import { getPlatformCapabilities } from '../../lib/platform';
import { useNarrowLayout } from '../../lib/useResponsiveLayout';
import type { HotkeyStatus, PlatformCapabilities } from '../../lib/types';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Btn, Card, Pill } from '../_atoms';
import { SettingRow } from './shared';
import { checkScreenCapturePermission, requestScreenCapturePermission } from '../../lib/ipc/permissions';
import { checkPermission, permissionAction, type PermissionCheck } from '../../lib/permissionOnboarding';
import { usePermissionRefresh } from '../../lib/usePermissionRefresh';


export function PermissionsSection() {
  const { t } = useTranslation();
  const [accessibility, setAccessibility] = useState<PermissionCheck>('loading');
  const [microphone, setMicrophone] = useState<PermissionCheck>('loading');
  const [screenCapture, setScreenCapture] = useState<PermissionCheck>('loading');
  const [permissionError, setPermissionError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [hotkey, setHotkey] = useState<HotkeyStatus | null>(null);
  const [platformCaps, setPlatformCaps] = useState<PlatformCapabilities | null>(null);
  const isMac = platformCaps?.platform === 'macos';
  const { capability } = useHotkeySettings();
  const stackLayout = useNarrowLayout();
  const permissionActionsStyle: CSSProperties = {
    display: 'flex',
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: stackLayout ? 'flex-start' : 'flex-end',
    width: '100%', flexWrap: 'wrap', minWidth: 0, gap: 8,
  };


  useEffect(() => {
    void getPlatformCapabilities().then(setPlatformCaps);
  }, []);

  const refreshPermissions = usePermissionRefresh(async active => {
    await Promise.all([
      checkPermission('accessibility', checkAccessibilityPermission, 'settings').then(v => { if (active()) setAccessibility(v); }),
      checkPermission('microphone', checkMicrophonePermission, 'settings').then(v => { if (active()) setMicrophone(v); }),
      checkPermission('screenCapture', isMac ? checkScreenCapturePermission : async () => 'notApplicable', 'settings').then(v => { if (active()) setScreenCapture(v); }),
    ]);
  });

  const refreshHotkey = async () => {
    setHotkey(await getHotkeyStatus());
  };

  useEffect(() => {
    refreshPermissions();
    if (platformCaps?.supportsDesktopHotkey === true) {
      refreshHotkey();
    }
  }, [platformCaps?.platform, platformCaps?.supportsDesktopHotkey, isMac]);


  const requestPermission = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setPermissionError(false);
    try {
      await action();
      await refreshPermissions();
    } catch {
      setPermissionError(true);
    } finally {
      setBusy(false);
    }
  };

  const reRequestAccessibility = async () => {
    const result = await requestAccessibilityPermission();
    // 如果 TCC 弹窗已拒绝（或之前已拒绝不再弹），引导用户到系统设置
    if (result !== 'granted') {
      await openSystemSettings('accessibility');
    }
    refreshPermissions();
  };

  const reRequestMicrophone = async () => {
    const status = await requestMicrophonePermission();
    if (status === 'denied' || status === 'restricted') await openSystemSettings('microphone');
    await refreshPermissions();
  };

  return (
    <Card>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 6 }}>
        <div style={{ fontSize: 13, fontWeight: 600 }}>{t('settings.permissions.title')}</div>
      </div>
      <SettingRow label={t('settings.permissions.micLabel')}>
        <div style={permissionActionsStyle}>
          <PermissionPill status={microphone} />
          {permissionAction(microphone) === 'retry' ? (
            <Btn variant="ghost" size="sm" onClick={refreshPermissions}>
              {t('common.retry')}
            </Btn>
          ) : permissionAction(microphone) === 'request' && (
            <Btn variant="ghost" size="sm" disabled={busy} onClick={() => void requestPermission(reRequestMicrophone)}>
              {microphone === 'denied' || microphone === 'restricted' ? t('settings.permissions.openSystem') : t('settings.permissions.grant')}
            </Btn>
          )}
        </div>
      </SettingRow>
      {(isMac || capability?.requiresAccessibilityPermission) && (
        <SettingRow label={t('settings.permissions.accLabel')}>
          <div style={permissionActionsStyle}>
            <PermissionPill status={accessibility} />
            {permissionAction(accessibility) !== 'none' && (
              <Btn variant="ghost" size="sm" disabled={busy} onClick={() => void (permissionAction(accessibility) === 'retry' ? refreshPermissions() : requestPermission(reRequestAccessibility))}>
                {permissionAction(accessibility) === 'retry' ? t('common.retry') : accessibility === 'denied' ? t('settings.permissions.openSystem') : t('settings.permissions.grant')}
              </Btn>
            )}

          </div>
        </SettingRow>
      )}
      {isMac && (
        <SettingRow label={t('settings.dataStorage.screenOcrPermissionLabel')} desc={t('onboarding.screenPermissionDesc')}>
          <div style={permissionActionsStyle}>
            <PermissionPill status={screenCapture} />
            {permissionAction(screenCapture) !== 'none' && (
              <Btn variant="ghost" size="sm" disabled={busy} onClick={() => void (permissionAction(screenCapture) === 'retry' ? refreshPermissions() : requestPermission(async () => {
                const status = await requestScreenCapturePermission();
                if (status !== 'granted' && status !== 'notApplicable') await openSystemSettings('screenCapture');
              }))}>
                {permissionAction(screenCapture) === 'retry' ? t('common.retry') : t('settings.permissions.grant')}
              </Btn>
            )}
          </div>
        </SettingRow>
      )}
      {permissionError && (
        <div role="alert">
          {t('common.operationFailed')}
          <Btn variant="ghost" size="sm" disabled={busy} onClick={refreshPermissions}>{t('common.retry')}</Btn>
        </div>
      )}
      {platformCaps?.supportsDesktopHotkey === true && (
      <SettingRow label={t('settings.permissions.hotkeyLabel')}>
        <div style={permissionActionsStyle}>
          {hotkey?.message && (
            <span style={{
              fontSize: 11.5, color: 'var(--ol-ink-4)',
              whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
              minWidth: 0, flex: '0 1 auto',
            }}>
              {hotkey.message}
            </span>
          )}
          <HotkeyStatusPill status={hotkey} />
        </div>
      </SettingRow>
      )}
    </Card>
  );
}

function PermissionPill({ status }: { status: PermissionCheck }) {
  const { t } = useTranslation();
  if (status === 'checkFailed' || status === 'unavailable') {
    return <Pill tone="outline">{t('settings.permissions.checkFailed')}</Pill>;
  }
  if (status === 'loading') {
    return <Pill tone="default">{t('settings.permissions.checking')}</Pill>;
  }
  if (status === 'granted') {
    return <Pill tone="ok"><Icon name="check" size={11} />{t('settings.permissions.granted')}</Pill>;
  }
  if (status === 'notApplicable') {
    return <Pill tone="default">{t('settings.permissions.notApplicable')}</Pill>;
  }
  if (status === 'noDevice') {
    return <Pill tone="outline">{t('settings.permissions.micNoDevice')}</Pill>;
  }
  if (status === 'denied' || status === 'restricted') {
    return <Pill tone="outline">{t('settings.permissions.denied')}</Pill>;
  }
  return <Pill tone="outline">{t('settings.permissions.indeterminate')}</Pill>;
}

function HotkeyStatusPill({ status }: { status: HotkeyStatus | null }) {
  const { t } = useTranslation();
  if (!status) {
    return <Pill tone="default">{t('settings.permissions.checking')}</Pill>;
  }
  if (status.state === 'installed') {
    return <Pill tone="ok"><Icon name="check" size={11} />{t('settings.permissions.hotkeyInstalled')}</Pill>;
  }
  if (status.state === 'starting') {
    return <Pill tone="default">{t('settings.permissions.hotkeyStarting')}</Pill>;
  }
  return <Pill tone="outline">{t('settings.permissions.hotkeyFailed')}</Pill>;
}
