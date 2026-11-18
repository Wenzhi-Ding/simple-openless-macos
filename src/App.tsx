import { lazy, Suspense, useEffect, useState } from 'react';
import { Capsule } from './components/Capsule';
import { detectOS, type OS } from './components/WindowChrome';
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
  getStartupSnapshot,
  getSettings,
  getPlatformCapabilities,
  isTauri,
} from './lib/ipc';
import type { PlatformCapabilities } from './lib/types';
import { checkScreenCapturePermission } from './lib/ipc/permissions';
import { HotkeySettingsProvider } from './state/HotkeySettingsContext';
import { checkPermission, SCREEN_PERMISSION_REVIEWED_KEY, screenPermissionNeedsReview } from './lib/permissionOnboarding';

// 各窗口/重页面懒加载,让每个 webview 只下载并解析自己用到的那部分代码。原本所有窗口
// (主设置 / 胶囊 / QA / Less Computer / glow)共用一个打包产物,导致 5 个常驻 WebKit
// 进程都把整套设置 UI(FloatingShell + 各设置页…)和聊天面板加载进来,
// 常驻内存离谱。拆开后胶囊/glow 这类轻窗口不再加载设置/聊天代码。胶囊保持 eager:
// 它是听写实时反馈、对首帧延迟敏感,且体积很小。
const FloatingShell = lazy(() =>
  import('./components/FloatingShell').then(m => ({ default: m.FloatingShell })),
);
const Onboarding = lazy(() =>
  import('./components/Onboarding').then(m => ({ default: m.Onboarding })),
);

interface AppProps {
  isCapsule: boolean;
  forcedOs?: OS | null;
}

type Gate = 'checking' | 'incompatible' | 'onboarding' | 'ready';

/**
 * 所有 Tauri webview 共用同一个 fail-closed 启动边界。胶囊、QA、预览和 Less Computer
 * 也会调用业务 IPC，不能因为它们不是主窗口就绕过 2.0 握手。requireBackendReady 内部
 * 复用同一个 Promise，所以主窗口后续读取不会产生第二次启动请求。
 */
export function App(props: AppProps) {
  const [ready, setReady] = useState(!isTauri);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri) return;
    void getStartupSnapshot()
      .then(() => setReady(true))
      .catch(reason => {
        const detail = reason instanceof Error ? reason.message : String(reason);
        console.error('[startup] backend contract handshake failed', reason);
        setError(detail);
      });
  }, []);

  if (error) {
    return <div role="alert">OpenLess Core 无法启动或版本不兼容。 {error}</div>;
  }
  if (!ready) {
    return <div role="status">正在检查 OpenLess Core 兼容性…</div>;
  }
  return <ReadyApp {...props} />;
}

function ReadyApp({ isCapsule, forcedOs }: AppProps) {
  if (isCapsule) {
    return <Capsule os={forcedOs} />;
  }
  const os = forcedOs ?? detectOS();
  // 权限探测期间显示启动门控，避免主界面误报授权状态。
  const [gate, setGate] = useState<Gate>(isTauri ? 'checking' : 'ready');
  const [startupError, setStartupError] = useState<string | null>(null);
  const [platformCaps, setPlatformCaps] = useState<PlatformCapabilities | null>(null);
  const completeOnboarding = () => {
    if (os === 'mac') {
      try { localStorage.setItem(SCREEN_PERMISSION_REVIEWED_KEY, '1'); }
      catch (error) { console.warn('[permissions] could not remember optional step', error); }
    }
    setGate('ready');
  };
  useEffect(() => {
    if (!isTauri) return;
    void getStartupSnapshot()
      .then(() => getPlatformCapabilities())
      .then(setPlatformCaps)
      .catch(error => {
        const detail = error instanceof Error ? error.message : String(error);
        console.error('[startup] backend contract handshake failed', error);
        setStartupError(detail);
        setGate('incompatible');
      });
  }, []);

  useEffect(() => {
    if (!isTauri || !platformCaps) return;
    let cancelled = false;
    requestAnimationFrame(() => {
      if (cancelled) return;
      (async () => {
        // 尊重 prefs.startMinimized：开了静默启动就别在前端强 show 主窗口。否则
        // Rust 端 setup() 抑制掉的窗口，会被这条 useEffect 在 webview 加载完成后
        // 再通过 IPC 拉出来 —— issue #468 在 Rust 修复后用户仍能在 Win11 上复现
        // 的最后一条路径（Rust log 里看不到，因为走的是 plugin-window 的 IPC）。
        try {
          const prefs = await getSettings();
          if (prefs.startMinimized) return;
        } catch (err) {
          // 安全侧默认 = 不弹窗。Rust 端 get_settings 签名是
          // `pub fn get_settings(...) -> UserPreferences`（非 Result），所以
          // 该 catch 唯一会被触发的场景是 Tauri IPC 基础设施抖动（autostart 早期
          // __TAURI_INTERNALS__ 还没就绪）。旧逻辑 fall-through to show 会在用户
          // 开了静默启动时仍把主窗口弹出来 —— #468 复现路径。
          //
          // 此时 tray 已由 Rust 端 setup() 在 webview 加载前注册完成，是稳定的
          // 兜底入口；宁可让用户从 tray 手动唤起，也不要在抖动时强 show 一个白色
          // / 透明主窗口。首次安装的"prefs 不存在"场景不走这里 —— Rust 端会返回
          // 默认 UserPreferences。
          const detail = err instanceof Error ? err.message : String(err);
          console.warn('[startup] read startMinimized failed; staying hidden to avoid #468:', detail, err);
          return;
        }
        const { getCurrentWindow } = await import('@tauri-apps/api/window');
        if (cancelled) return;
        const currentWindow = getCurrentWindow();
        if (!(await currentWindow.isVisible())) {
          await currentWindow.show();
        }
      })().catch(error => console.warn('[startup] show main window failed', error));
    });
    return () => {
      cancelled = true;
    };
  }, [os, platformCaps]);

  useEffect(() => {
    if (!isTauri || !platformCaps) return;
    let cancelled = false;

    void (async () => {
      const [a, m] = await Promise.all([
        checkPermission('accessibility', checkAccessibilityPermission, 'startup'),
        checkPermission('microphone', checkMicrophonePermission, 'startup'),
      ]);
      if (cancelled) return;
      const aOk = a === 'granted' || a === 'notApplicable';
      // noDevice（当前没有麦克风）不是权限问题：不卡 onboarding，
      // 让用户进应用后在权限页看到“未检测到麦克风”的明确提示。见 issue #779。
      const mOk = m === 'granted' || m === 'notApplicable' || m === 'noDevice';
      let screenReview = false;
      if (os === 'mac' && localStorage.getItem(SCREEN_PERMISSION_REVIEWED_KEY) !== '1') {
        // A failed optional check still offers the unified retry/skip UI.
        const screen = await checkPermission('screenCapture', checkScreenCapturePermission, 'startup');
        screenReview = screenPermissionNeedsReview(true, false, screen);
      }
      if (cancelled) return;
      setGate(aOk && mOk && !screenReview ? 'ready' : 'onboarding');
    })().catch(error => {
      console.warn('[startup] permission gate failed', error);
      if (!cancelled) {
        setGate('ready');
      }
    });

    return () => {
      cancelled = true;
    };
  }, [os, platformCaps]);

  if (gate === 'checking') {
    return <div role="status">正在检查 OpenLess Core 兼容性…</div>;
  }
  if (gate === 'incompatible') {
    return (
      <div role="alert">
        OpenLess Core 无法启动或版本不兼容。{startupError ? ` ${startupError}` : ''}
      </div>
    );
  }

  return (
    <Suspense fallback={null}>
      <HotkeySettingsProvider>
        {gate === 'onboarding' ? (
          <Onboarding onComplete={completeOnboarding} />
        ) : (
          <FloatingShell os={os} />
        )}
      </HotkeySettingsProvider>
    </Suspense>
  );
}
