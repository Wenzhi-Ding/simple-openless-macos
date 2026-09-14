// FloatingShell.tsx — frosted outer frame + raised inner console.
// Sidebar lives INSIDE the console card.
// Settings opens as a centered modal sheet from the sidebar bottom entry.
//
// Ported verbatim from design_handoff_openless/variants.jsx::FloatingShell.

import { useEffect, useState, type ComponentType, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Icon } from './Icon';
import { Tooltip } from './Tooltip';
import { WindowChrome, detectOS, type OS } from './WindowChrome';
import { AudioCueListener } from "./AudioCue";
import { SettingsModal } from './SettingsModal';
import { Overview } from '../pages/Overview';
import { History } from '../pages/History';
import { Vocab } from '../pages/Vocab';
// 风格市场（Marketplace）现在是侧栏「风格」展开组下的独立页面（不再是 Style 页面内 modal）。
// LocalAsr 不再作为主 nav tab——本地 ASR 模型管理已合并到 Settings → Advanced 中
// 通过 <LocalAsr embedded /> 渲染。这里之前的 import 与 NAV_BASE 条目都已移除。
import { APP_VERSION_LABEL, IS_BETA_BUILD } from '../lib/appVersion';
import { applyFontScale, readFontScale } from '../lib/fontScale';
import { type SettingsSectionId } from './SettingsModal';
import { useNarrowLayout } from '../lib/useResponsiveLayout';
import { useAppState, type AppTab } from '../state/useAppState';


/** macOS 上侧栏顶部需让开原生红绿灯的高度（红绿灯竖直落在 ~6–22px）。 */
const MAC_TRAFFIC_LIGHT_CLEARANCE = 30;
const SIDEBAR_WIDTH = 188;

/** tab → 页面组件映射（渲染主内容用；与侧栏树解耦，含未直接列在树上的页）。 */
const PAGE_CMP: Record<AppTab, ComponentType> = {
  overview: Overview,
  history: History,
  vocab: Vocab,
};

/** Current product navigation: the four directly reachable pages. */
const NAV_ITEMS: Array<{ id: AppTab; icon: string }> = [
  { id: 'overview', icon: 'overview' },
  { id: 'history', icon: 'history' },
  { id: 'vocab', icon: 'vocab' },
];

interface FloatingShellProps {
  os?: OS;
  initialTab?: AppTab;
  initialSettings?: boolean;
}

export function FloatingShell({ os: osProp, initialTab = 'overview', initialSettings = false }: FloatingShellProps) {
  const os = osProp ?? detectOS();
  return (
    <WindowChrome os={os} title="OpenLess" height="100%">
      <FloatingShellBody os={os} initialTab={initialTab} initialSettings={initialSettings} />
    </WindowChrome>
  );
}

function FloatingShellBody({ os, initialTab, initialSettings }: { os: OS; initialTab: AppTab; initialSettings: boolean }) {
  const { t } = useTranslation();
  const narrow = useNarrowLayout();
  const { currentTab, setCurrentTab, settingsOpen, setSettingsOpen } = useAppState(initialTab, initialSettings);
  const [settingsInitialSection, setSettingsInitialSection] = useState<SettingsSectionId | undefined>();

  // tab 切换的 cross-fade：旧页 blur+fade out（180ms），结束后挂载新页（走 ol-page-slide enter）。
  // displayTab 是实际渲染的 tab，currentTab 是用户点中的目标 tab。
  const [displayTab, setDisplayTab] = useState<AppTab>(initialTab);
  const [tabPhase, setTabPhase] = useState<'idle' | 'exiting'>('idle');
  useEffect(() => {
    if (currentTab === displayTab) return;
    setTabPhase('exiting');
    const id = window.setTimeout(() => {
      setDisplayTab(currentTab);
      setTabPhase('idle');
    }, 180);
    return () => window.clearTimeout(id);
  }, [currentTab, displayTab]);

  // 字体档位 — 启动时按 localStorage 应用一次；之后改动来自 Settings 的"个性化"section。
  useEffect(() => {
    applyFontScale(readFontScale());
  }, []);

  const Page = PAGE_CMP[displayTab] ?? Overview;

  const openSettings = (section?: SettingsSectionId) => {
    setSettingsInitialSection(section);
    setSettingsOpen(true);
  };

  // ⌘, 打开设置页面
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === ',') {
        e.preventDefault();
        openSettings();
      }
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, []);

  const narrowTitle = settingsOpen ? t('shell.footer.settings') : t(`nav.${currentTab}`);

  return (
    // 不再为 macOS 红绿灯预留顶部 28px 空条（用户反馈「块上方多一条丑横条」）：
    // 侧栏与内容块都顶到窗口最上沿，原生红绿灯直接浮在侧栏左上角的块面上。
    // 侧栏内 brand 行在 mac 上加 topClearance 让「OpenLess」避开红绿灯。
    <div style={{ flex: 1, position: 'relative', display: 'flex', flexDirection: 'column', minHeight: 0, paddingTop: 0, background: 'var(--ol-app-shell-bg)' }}>

      {narrow && (
        <NarrowTopBar
          title={narrowTitle}
          onOpenSettings={() => openSettings()}
          settingsActive={settingsOpen}
        />
      )}

      {/* Main shell — flush with the frosted backplate (no separate float). */}
      <div
        data-ol-settings-open={settingsOpen ? 'true' : undefined}
        className="ol-app-shell-bg"
        style={{
          flex: 1, minHeight: 0,
          display: 'flex',
          overflow: 'hidden',
          position: 'relative',
          zIndex: 1,
        }}>

        {/* Sidebar — desktop / wide only。 */}
        {!narrow && (
        <aside
          className="ol-sidebar-surface"
          style={{
            width: SIDEBAR_WIDTH,
            height: '100%',
            flexShrink: 0,
            display: 'flex', flexDirection: 'column',
            background: 'var(--ol-sidebar-bg)',
            borderRight: '0.5px solid var(--ol-line)',
            // mac：顶部空出红绿灯高度，让 brand/nav 落在红绿灯下方。
            padding: os === 'mac' ? `${MAC_TRAFFIC_LIGHT_CLEARANCE}px 10px 12px` : '10px 10px 12px',
          }}>

          {/* brand */}
          <div style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '2px 8px 12px' }}>
            <img
              src="AppIcon.png"
              alt="OpenLess"
              style={{ width: 22, height: 22, borderRadius: 5, flexShrink: 0, boxShadow: '0 1px 2px rgba(0,0,0,.1), 0 0 0 0.5px rgba(0,0,0,.06)' }} />

            <span style={{ flex: 1, fontSize: 13.5, fontWeight: 600, letterSpacing: '-0.01em', color: 'var(--ol-ink)', whiteSpace: 'nowrap' }}>OpenLess</span>
          </div>

          <nav aria-label={t('shell.footer.settings')} style={{ display: 'flex', flexDirection: 'column', gap: 1 }}>
            {NAV_ITEMS.map(({ id, icon }) => {
              const active = !settingsOpen && currentTab === id;
              return (
                <Tooltip key={id} content={t(`shell.navHint.${id}`)} placement="right">
                  <button onClick={() => setCurrentTab(id)} className={active ? 'ol-nav-btn ol-nav-btn-active' : 'ol-nav-btn'} style={navBtnStyle}>
                    <Icon name={icon} size={14} />
                    <span style={{ flex: 1 }}>{t(`nav.${id}`)}</span>
                  </button>
                </Tooltip>
              );
            })}
          </nav>

          <div style={{ flex: 1 }} />

          {/* 底部两行：上行 = 版本 chip（含 BETA 标），下行 = 设置按钮。
              单行布局在窄 sidebar 下会把「设置」挤成两行竖字 + 版本糊一起；
              翻回两行同时把顺序反过来：设置真正落到最底，版本在它上面。 */}
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, paddingTop: 10 }}>
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 8,
                flexWrap: 'wrap',
                padding: '0 10px',
                fontFamily: 'var(--ol-font-sans)',
                fontSize: 11,
                color: 'var(--ol-ink-4)',
              }}
            >
              {IS_BETA_BUILD && (
                <span style={{
                  display: 'inline-block',
                  padding: '2px 8px',
                  fontSize: 10,
                  fontWeight: 600,
                  letterSpacing: '0.04em',
                  textTransform: 'uppercase',
                  color: 'var(--ol-blue)',
                  background: 'var(--ol-blue-soft)',
                  border: '0.5px solid var(--ol-pill-blue-border)',
                  borderRadius: 999,
                }}>{t('shell.betaTag')}</span>
              )}

              <span>{t('shell.footer.version', { version: APP_VERSION_LABEL })}</span>
            </div>

            <Tooltip content={t('shell.navHint.settings')} placement="right">
              <button
                onClick={() => openSettings()}
                className={settingsOpen ? 'ol-nav-btn ol-nav-btn-active' : 'ol-nav-btn'}
                style={{
                  display: 'flex', alignItems: 'center', gap: 10,
                  padding: '7px 10px',
                  borderRadius: 8, border: 0,
                  fontFamily: 'inherit', fontSize: 13,
                  cursor: 'default',
                  transition: 'color 0.16s var(--ol-motion-quick), background 0.16s var(--ol-motion-quick)',
                  textAlign: 'left',
                }}
              >
                <Icon name="settings" size={14} />
                <span style={{ flex: 1 }}>{t('shell.footer.settings')}</span>
              </button>
            </Tooltip>
          </div>
        </aside>
        )}

        {/* Main content — 平铺实底块（用户反馈：右侧要一整块、不要圆角、别浮成卡）。
            surface 实底，无外边距无圆角无阴影，直接顶到窗口边缘，与侧栏之间仅靠
            侧栏的 border-right 分隔（即「侧栏那套逻辑」）。mac 圆角由原生窗口裁切。 */}
        <div style={{ flex: 1, minWidth: 0, padding: 0, display: 'flex' }}>
          <main
            className="ol-console-main"
            style={{
              flex: 1, minWidth: 0,
              overflow: 'hidden',
              background: 'var(--ol-surface)',
              borderRadius: 0,
              border: 'none',
              boxShadow: 'none',
              display: 'flex',
              flexDirection: 'column',
            }}
          >
            {/* key={displayTab} 让每次切换重挂这棵子树 → ol-page-slide keyframe 重新触发。
                旧 tab 退出时不立刻 unmount，而是先播 ol-page-fadeout（blur+淡出），
                180ms 后再切到新 tab 并播入场动画。详见 displayTab/tabPhase 的 effect。
                padding + overflow:auto 直接挂在这棵 wrapper 上：
                  - 自然高度的页（Overview / Vocab / Style）—— 整页内容超出时 wrapper 出现滚动条
                  - 用 height:100% 撑满的页（History 左右双列）—— 100% 能解析到 wrapper 的固定高度，
                    两列内部各自的 overflow:auto 才能独立滚动 */}
            <div
              key={displayTab}
              // issue #243：所有 tab 都允许 overflow:auto，让窗口被压缩 / 文案
              //   变长时仍可触达底部内容（Codex P1：之前 overview 用 hidden
              //   会让缩窗后 Recent 卡彻底不可见）。
              //   - Overview 借 Overview.tsx 内部 flex 把底部行 grow 到撑满，
              //     正常尺寸下内容刚好占满 → 浏览器自动不显示 scrollbar；
              //     真挤不下了才 fallback 出细滚动条。
              //   - 其他 tab 同样走细滚动条。
              className="ol-thinscroll ol-scroll-fade"
              style={{
                flex: 1, minHeight: 0,
                overflow: 'auto',
                padding: narrow
                  ? '16px 16px calc(16px + env(safe-area-inset-bottom, 0px) + 56px)'
                  : '24px 28px 32px',
                // position:relative 让页面里的"已保存"toast 用 absolute top:16 right:16
                // 锚到这块控制台卡的右上角，而不是横在页头变成长横幅。
                position: 'relative',
                animation: narrow
                  ? (tabPhase === 'exiting'
                    ? 'ol-page-fadeout-narrow 0.18s var(--ol-motion-soft) forwards'
                    : 'ol-page-fade-narrow 0.22s var(--ol-motion-soft) both')
                  : (tabPhase === 'exiting'
                    ? 'ol-page-fadeout 0.18s var(--ol-motion-soft) forwards'
                    : 'ol-page-slide 0.34s var(--ol-motion-spring) both'),
                willChange: narrow ? 'opacity' : 'opacity, transform',
                display: 'flex',
                flexDirection: 'column',
              }}
            >
              {displayTab === 'overview' ? (
                <Overview onOpenHistory={() => setCurrentTab('history')} />
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
                  <Page />
                </div>
              )}
            </div>
          </main>
        </div>
      </div>

            {/* Settings modal — rendered inside this window */}
      {settingsOpen &&
        <SettingsModal
          key={settingsInitialSection ?? 'default'}
          os={os}
          initialSettingsSection={settingsInitialSection}
          onClose={() => setSettingsOpen(false)}
        />
      }

      <AudioCueListener />
      {narrow && <NarrowBottomNav currentTab={currentTab} settingsOpen={settingsOpen} onSelectTab={setCurrentTab} onOpenSettings={() => openSettings()} />}

      {/* tab switch animation keyframes */}
      <style>{`
        /* nav 三段视觉层次（扁平静态，无 pill 滑块）：
             基础态  → ink-3（中灰文字 + 透明底）
             hover  → ink（深色文字 + surface-2 浅灰底）  ← 让"翻译"等字词在悬停时高亮
             选中  → ink（深色文字加粗 + surface-2 静态圆角底）
           全走 class：sidebar 按钮不写 inline background，:hover / active 底色才能生效
           （CSS 不能盖 inline style）；其他 .ol-nav-btn 使用方仍带 inline background，
           这里的 active 底色对它们不生效，维持各自原样。 */
        .ol-nav-btn {
          color: var(--ol-ink-3);
          font-weight: 500;
        }
        .ol-nav-btn.ol-nav-btn-active {
          color: var(--ol-ink);
          font-weight: 600;
          background: var(--ol-surface-2);
        }
        .ol-nav-btn:not(.ol-nav-btn-active):hover {
          background: var(--ol-surface-2);
          color: var(--ol-ink);
        }
        /* 只动 opacity/transform（合成器友好）：filter:blur 每帧全页重算，
           是 tab 切换「卡」的元凶，已移除。 */
        @keyframes ol-page-slide {
          from { opacity: 0; transform: translate3d(10px, 0, 0) scale(.996); }
          to   { opacity: 1; transform: translate3d(0, 0, 0) scale(1); }
        }
        @keyframes ol-page-fadeout {
          from { opacity: 1; transform: translate3d(0, 0, 0); }
          to   { opacity: 0; transform: translate3d(-6px, 0, 0); }
        }
        @keyframes ol-page-fade-narrow {
          from { opacity: 0; }
          to   { opacity: 1; }
        }
        @keyframes ol-page-fadeout-narrow {
          from { opacity: 1; }
          to   { opacity: 0; }
        }
        @keyframes ol-narrow-sheet-backdrop {
          from { opacity: 0; }
          to   { opacity: 1; }
        }
        @keyframes ol-narrow-sheet-up {
          from { opacity: 0; transform: translate3d(0, 12px, 0); }
          to   { opacity: 1; transform: translate3d(0, 0, 0); }
        }
        @keyframes ol-prompt-fade {
          from { opacity: 0; backdrop-filter: blur(0); -webkit-backdrop-filter: blur(0); }
          to   { opacity: 1; backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px); }
        }
        @keyframes ol-prompt-pop {
          from { opacity: 0; transform: translateY(6px) scale(.97); filter: blur(6px); }
          to   { opacity: 1; transform: translateY(0) scale(1); filter: blur(0); }
        }
      `}</style>
    </div>
  );
}

const NARROW_NAV_ITEMS: Array<{ id: AppTab; icon: string }> = [
  ...NAV_ITEMS,
];

function NarrowTopBar({
  title,
  onOpenSettings,
  settingsActive,
}: {
  title: string;
  onOpenSettings: () => void;
  settingsActive: boolean;
}) {
  const { t } = useTranslation();
  return (
    <header
      style={{
        flexShrink: 0,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: 12,
        padding: 'calc(10px + env(safe-area-inset-top, 0px)) 14px 10px',
        borderBottom: '0.5px solid var(--ol-line-soft)',
        background: 'var(--ol-surface)',
        zIndex: 2,
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
        <img
          src="AppIcon.png"
          alt=""
          style={{ width: 22, height: 22, borderRadius: 5, flexShrink: 0 }}
        />
        <span
          style={{
            fontSize: 16,
            fontWeight: 600,
            letterSpacing: '-0.02em',
            color: 'var(--ol-ink)',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
        >
          {title}
        </span>
      </div>
      <button
        type="button"
        onClick={onOpenSettings}
        aria-label={t('shell.footer.settings')}
        className={settingsActive ? 'ol-nav-btn ol-nav-btn-active' : 'ol-nav-btn'}
        style={{
          width: 36,
          height: 36,
          flexShrink: 0,
          border: 0,
          borderRadius: 10,
          background: settingsActive ? 'var(--ol-surface-2)' : 'transparent',
          display: 'inline-flex',
          alignItems: 'center',
          justifyContent: 'center',
          cursor: 'default',
        }}
      >
        <Icon name="settings" size={18} />
      </button>
    </header>
  );
}

function NarrowBottomNav({ currentTab, settingsOpen, onSelectTab, onOpenSettings }: {
  currentTab: AppTab;
  settingsOpen: boolean;
  onSelectTab: (tab: AppTab) => void;
  onOpenSettings: () => void;
}) {
  const { t } = useTranslation();
  return (
    <nav aria-label={t('shell.footer.settings')} style={{ position: 'absolute', left: 0, right: 0, bottom: 0, zIndex: 55, display: 'flex', alignItems: 'stretch', justifyContent: 'space-around', gap: 4, padding: '6px 8px calc(6px + env(safe-area-inset-bottom, 0px))', borderTop: '0.5px solid var(--ol-line-soft)', background: 'var(--ol-surface)' }}>
      {NARROW_NAV_ITEMS.map(({ id, icon }) => {
        const active = !settingsOpen && currentTab === id;
        return <button key={id} type="button" onClick={() => onSelectTab(id)} className={active ? 'ol-nav-btn ol-nav-btn-active' : 'ol-nav-btn'} style={narrowNavBtnStyle}><Icon name={icon} size={18} /><span style={{ fontSize: 10.5, fontWeight: active ? 600 : 500 }}>{t(`nav.${id}`)}</span></button>;
      })}
      <button type="button" onClick={onOpenSettings} className={settingsOpen ? 'ol-nav-btn ol-nav-btn-active' : 'ol-nav-btn'} style={narrowNavBtnStyle}><Icon name="settings" size={18} /><span style={{ fontSize: 10.5 }}>{t('shell.footer.settings')}</span></button>
    </nav>
  );
}

const narrowNavBtnStyle: CSSProperties = {
  flex: 1,
  minWidth: 0,
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'center',
  justifyContent: 'center',
  gap: 4,
  padding: '6px 4px',
  border: 0,
  borderRadius: 10,
  background: 'transparent',
  fontFamily: 'inherit',
  cursor: 'default',
};

/** 侧栏导航按钮基础样式（扁平项 / 分组标题 / 子项共用；子项额外覆盖 paddingLeft）。 */
const navBtnStyle: CSSProperties = {
  display: 'flex', alignItems: 'center', gap: 10,
  width: '100%',
  padding: '7px 10px',
  borderRadius: 8, border: 0,
  fontFamily: 'inherit', fontSize: 13,
  cursor: 'default',
  transition: 'color 0.16s var(--ol-motion-quick), background 0.16s var(--ol-motion-quick)',
  textAlign: 'left',
};
