// Onboarding.tsx — first-run permission and service setup.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
  getPlatformCapabilities,
  openSystemSettings,
  requestAccessibilityPermission,
  requestMicrophonePermission,
} from '../lib/ipc';
import { defaultDictationHotkey, formatComboLabel } from '../lib/hotkey';
import { requiresScreenCapturePermission } from '../lib/platform';
import { checkPermission, permissionAction, permissionStatusKey, type PermissionCheck, dictationPermissionsReady, permissionGranted } from '../lib/permissionOnboarding';
import { usePermissionRefresh } from '../lib/usePermissionRefresh';
import { checkScreenCapturePermission, requestScreenCapturePermission } from '../lib/ipc/permissions';
import type { PermissionStatus, PlatformCapabilities } from '../lib/types';
import { useHotkeySettings } from '../state/HotkeySettingsContext';

interface OnboardingProps {
  onComplete: () => void;
}

export function Onboarding({ onComplete }: OnboardingProps) {
  const { t } = useTranslation();
  const [platformCaps, setPlatformCaps] = useState<PlatformCapabilities | null>(null);

  useEffect(() => {
    void getPlatformCapabilities().then(setPlatformCaps);
  }, []);

  if (!platformCaps) {
    return <OnboardingLoading label={t('common.loading')} />;
  }


  return <DesktopOnboarding onComplete={onComplete} platformCaps={platformCaps} />;
}

function DesktopOnboarding({
  onComplete,
  platformCaps,
}: OnboardingProps & { platformCaps: PlatformCapabilities }) {
  const { t } = useTranslation();
  const [accessibility, setAccessibility] = useState<PermissionCheck>('loading');
  const [microphone, setMicrophone] = useState<PermissionCheck>('loading');
  const [screenCapture, setScreenCapture] = useState<PermissionCheck>('loading');
  const [permissionError, setPermissionError] = useState(false);
  const [busy, setBusy] = useState(false);
  const isMac = requiresScreenCapturePermission(platformCaps);
  const { capability, prefs } = useHotkeySettings();
  const dictationHotkeyLabel = formatComboLabel(prefs?.dictationHotkey ?? defaultDictationHotkey());
  const requiresAccessibility = isMac || !!capability?.requiresAccessibilityPermission;
  const refresh = usePermissionRefresh(async active => {
    const [a, m, s] = await Promise.all([
      checkPermission('accessibility', checkAccessibilityPermission, 'onboarding').then(v => { if (active()) setAccessibility(v); return v; }),
      checkPermission('microphone', checkMicrophonePermission, 'onboarding').then(v => { if (active()) setMicrophone(v); return v; }),
      checkPermission('screenCapture', isMac ? checkScreenCapturePermission : async () => 'notApplicable', 'onboarding').then(v => { if (active()) setScreenCapture(v); return v; }),
    ]);
    if (active() && dictationPermissionsReady(a, m) && permissionGranted(s)) onComplete();
  });
  const request = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setPermissionError(false);
    try { await action(); } catch { setPermissionError(true); }
    finally { await refresh(); setBusy(false); }
  };
  const onGrantAccessibility = () => request(async () => {
    if (!permissionGranted(await requestAccessibilityPermission())) await openSystemSettings('accessibility');
  });
  const onRequestMicrophone = () => request(async () => {
    if (microphone === 'denied' || microphone === 'restricted') await openSystemSettings('microphone');
    else {
      const status = await requestMicrophonePermission();
      if (status === 'denied' || status === 'restricted') await openSystemSettings('microphone');
    }
  });
  const onRequestScreenCapture = () => request(async () => {
    if (!permissionGranted(await requestScreenCapturePermission())) await openSystemSettings('screenCapture');
  });
  const label = (status: PermissionCheck) => permissionAction(status) === 'retry' ? t('common.retry')
    : permissionAction(status) === 'request' ? t('settings.permissions.grant')
    : t('settings.permissions.' + permissionStatusKey(status));
  const problems = [
    [accessibility, t('settings.permissions.accLabel')],
    [microphone, t('settings.permissions.micLabel')],
    ...(isMac ? [[screenCapture, t('settings.dataStorage.screenOcrPermissionLabel')]] : []),
  ].filter(([status]) => !permissionGranted(status as PermissionCheck)).map(([, name]) => name);

  return (
    <OnboardingSurface>
      <div
        style={{
          width: 'min(520px, 100%)',
          padding: 32,
          boxSizing: 'border-box',
          background: 'var(--ol-surface)',
          borderRadius: 14,
          border: '0.5px solid var(--ol-line)',
          boxShadow: 'var(--ol-shadow-lg)',
        }}
      >
        <BrandHeader title={t('settings.permissions.title')} desc={problems.length ? t('permissions.needsAttention', { names: problems.join('、') }) : t('settings.permissions.granted')} />

        {requiresAccessibility && (
          <PermissionStep index={1} title={t('settings.permissions.accLabel')}
            desc={t('onboarding.accessibilityDesc', { trigger: dictationHotkeyLabel })}
            status={accessibility} actionLabel={label(accessibility)}
            onAction={permissionAction(accessibility) === 'retry' ? refresh : onGrantAccessibility}
            disabled={busy || permissionAction(accessibility) === 'none'} />
        )}
        <PermissionStep index={2} title={t('onboarding.micTitle')} desc={t('onboarding.micDesc')}
          status={microphone} actionLabel={label(microphone)}
          onAction={permissionAction(microphone) === 'retry' ? refresh : onRequestMicrophone}
          disabled={busy || permissionAction(microphone) === 'none'}
          hint={microphone === 'noDevice' ? t('onboarding.micNoDeviceHint') : undefined} />
        {isMac && (
          <PermissionStep index={3} title={t('settings.dataStorage.screenOcrPermissionLabel')} desc={t('onboarding.screenPermissionDesc')}
            status={screenCapture} actionLabel={label(screenCapture)}
            onAction={permissionAction(screenCapture) === 'retry' ? refresh : onRequestScreenCapture}
            disabled={busy || permissionAction(screenCapture) === 'none'} />
        )}
        {permissionError && (
          <div role="alert" style={footerHintStyle}>
            {t('common.operationFailed')}{' '}
            <button type="button" onClick={() => void refresh()} disabled={busy}>{t('common.retry')}</button>
          </div>
        )}
        <div style={footerHintStyle}>
          {t('onboarding.footerHint')}
        </div>
        <button
          type="button"
          onClick={onComplete}
          style={{
            width: '100%',
            marginTop: 12,
            padding: '9px 14px',
            fontSize: 12.5,
            fontWeight: 500,
            fontFamily: 'inherit',
            border: '0.5px solid var(--ol-line-strong)',
            borderRadius: 8,
            background: 'var(--ol-surface)',
            color: 'var(--ol-ink-2)',
            cursor: 'pointer',
          }}
        >
          {isMac && dictationPermissionsReady(accessibility, microphone)
            ? t('onboarding.skipScreenPermission')
            : t('onboarding.continueToSettings')}
        </button>
      </div>
    </OnboardingSurface>
  );
}

function OnboardingLoading({ label }: { label: string }) {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        fontFamily: 'var(--ol-font-sans)',
        color: 'var(--ol-ink-3)',
        fontSize: 13,
      }}
    >
      {label}
    </div>
  );
}

function OnboardingSurface({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        overflow: 'auto',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 'calc(18px + env(safe-area-inset-top, 0px)) 16px calc(18px + env(safe-area-inset-bottom, 0px))',
        boxSizing: 'border-box',
        fontFamily: 'var(--ol-font-sans)',
      }}
    >
      {children}
    </div>
  );
}

function BrandHeader({ title, desc, compact = false }: { title: string; desc: string; compact?: boolean }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: compact ? 12 : 14, marginBottom: compact ? 4 : 18 }}>
      <img
        src="AppIcon.png"
        alt="OpenLess"
        style={{
          width: compact ? 48 : 52,
          height: compact ? 48 : 52,
          borderRadius: compact ? 12 : 13,
          flexShrink: 0,
        }}
      />
      <div style={{ minWidth: 0 }}>
        <div style={{ fontSize: compact ? 17 : 18, fontWeight: 650 }}>{title}</div>
        <div style={{ fontSize: 12.5, color: 'var(--ol-ink-3)', marginTop: 2, lineHeight: 1.45 }}>
          {desc}
        </div>
      </div>
    </div>
  );
}

interface StepProps {
  index: number;
  title: string;
  desc: string;
  status: PermissionCheck;
  actionLabel: string;
  onAction: () => void;
  disabled: boolean;
  hint?: string;
}

function PermissionStep({ index, title, desc, status, actionLabel, onAction, disabled, hint }: StepProps) {
  const { t } = useTranslation();
  const granted = status === 'granted' || status === 'notApplicable';
  return (
    <div
      style={{
        padding: '14px 0',
        borderTop: '0.5px solid var(--ol-line-soft)',
        display: 'flex',
        gap: 14,
        alignItems: 'flex-start',
      }}
    >
      <div
        style={{
          width: 22,
          height: 22,
          borderRadius: 999,
          background: granted ? 'var(--ol-blue)' : 'rgba(0,0,0,0.06)',
          color: granted ? '#fff' : 'var(--ol-ink-3)',
          display: 'inline-flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontSize: 11,
          fontWeight: 600,
          flexShrink: 0,
        }}
      >
        {granted ? '✓' : index}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 13.5, fontWeight: 600 }}>{title}</div>
        <div role="status" style={{ fontSize: 12, color: status === 'granted' ? 'var(--ol-green, #16834a)' : 'var(--ol-ink-3)' }}>
          {t('settings.permissions.' + permissionStatusKey(status))}
        </div>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-3)', marginTop: 3, lineHeight: 1.5 }}>{desc}</div>
        {hint && (
          <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', marginTop: 4, lineHeight: 1.5 }}>
            {hint.split('**').map((seg, i) => (i % 2 === 0 ? seg : <b key={i} style={{ color: 'var(--ol-ink-2)' }}>{seg}</b>))}
          </div>
        )}
      </div>
      <button
        onClick={disabled ? undefined : onAction}
        disabled={disabled}
        style={{
          flexShrink: 0,
          padding: '7px 14px',
          fontSize: 12.5,
          fontWeight: 500,
          fontFamily: 'inherit',
          border: 0,
          borderRadius: 8,
          background: granted ? 'var(--ol-surface-2)' : 'var(--ol-primary-solid-bg)',
          color: granted ? 'var(--ol-ink-3)' : 'var(--ol-primary-solid-ink)',
          cursor: disabled ? 'not-allowed' : 'default',
          opacity: disabled && !granted ? 0.6 : 1,
          transition: 'background 0.16s var(--ol-motion-quick), color 0.16s var(--ol-motion-quick), opacity 0.18s var(--ol-motion-soft), transform 0.12s var(--ol-motion-quick)',
        }}
      >
        {actionLabel}
      </button>
    </div>
  );
}

const footerHintStyle = {
  marginTop: 18,
  padding: '12px 14px',
  borderRadius: 8,
  background: 'var(--ol-surface-2)',
  fontSize: 11.5,
  color: 'var(--ol-ink-3)',
  lineHeight: 1.6,
} as const;
