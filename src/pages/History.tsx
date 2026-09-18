// History.tsx — 接 Tauri 后端 list_history / delete_history_entry / clear_history。
// 真实数据来自 ~/Library/Application Support/OpenLess/history.json。

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Icon } from '../components/Icon';
import { Tooltip } from '../components/Tooltip';
import { detectOS } from '../components/WindowChrome';
import { formatComboLabel } from '../lib/hotkey';
import { clearHistory, deleteHistoryEntry, listHistory, readAudioRecording, repolish, retranscribeRecording, isTauri } from '../lib/ipc';
import {
  contextStatusLabelKey,
  contextSummary,
  diagnosticsHeadlineParts,
  formatPolishDiagnosticsJson,
  outcomeLabelKey,
  stageLabelKey,
} from '../lib/polish-diagnostics';
import type { PolishContextDiagnostics, PolishDiagnostics as PolishDiagnosticsValue } from '../lib/types';
import { useNarrowLayout } from '../lib/useResponsiveLayout';
import type { DictationSession } from '../lib/types';
import { countCodePoints } from '../lib/unicode';
import { useHotkeySettings } from '../state/HotkeySettingsContext';
import { Btn, Card, PageHeader, Pill } from './_atoms';

// Truncate long diagnostic status labels without squeezing neighboring actions.
const TRUNCATED_PILL_STYLE = {
  minWidth: 0,
  maxWidth: '100%',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  display: 'block',
  flexShrink: 1,
} as const;

export function History() {
  const { t } = useTranslation();
  const os = detectOS();
  const [query, setQuery] = useState('');
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const [items, setItems] = useState<DictationSession[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [justCopied, setJustCopied] = useState(false);
  const [justCopiedRaw, setJustCopiedRaw] = useState(false);
  // 「重新转录」进行中：禁用按钮 + 显示「转录中…」，避免重复点击发起多次 ASR。
  const [retranscribing, setRetranscribing] = useState(false);
  // 录音文件 lazily-detected missing 状态：retention / 条数 cap 清理后磁盘上 wav
  // 可能已被删，但 history 条目 hasAudioRecording 仍写 true。任一组件
  // （播放 / 导出）首次 IPC 拿到 'recording not found' 时把 id 加进来，
  // 之后渲染按钮的条件就转 false，避免反复点击得到同样的 error。
  // 修 pr_agent "Missing file check" 反馈。
  const [audioMissingIds, setAudioMissingIds] = useState<Set<string>>(() => new Set());
  const markAudioMissing = useCallback((id: string) => {
    setAudioMissingIds(prev => {
      if (prev.has(id)) return prev;
      const next = new Set(prev);
      next.add(id);
      return next;
    });
  }, []);
  const { prefs } = useHotkeySettings();
  const narrow = useNarrowLayout();
  const [narrowDetailOpen, setNarrowDetailOpen] = useState(false);
  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const data = await listHistory();
      setItems(data);
      setActionError(null);
      setSelectedId(prev => (prev && data.some(s => s.id === prev) ? prev : data[0]?.id ?? null));
    } catch (error) {
      console.error('[history] failed to load history', error);
      setLoadError(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchShortcut = os === 'mac' ? '⌘K' : 'Ctrl+K';

  // 搜索词防抖：随输入实时更新 query，300ms 后落到 debouncedQuery 再过滤，
  // 避免每个按键都重算整张列表。
  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(query), 300);
    return () => window.clearTimeout(id);
  }, [query]);

  // ⌘K / Ctrl+K 聚焦搜索框（设计稿提示的快捷键）；⌘R / Ctrl+R 刷新历史列表
  // （与浏览器「重新加载」直觉一致）。preventDefault 拦掉 webview 默认的整页
  // reload，改为只重拉 listHistory，避免整个前端重挂载。
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        searchInputRef.current?.focus();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === 'r' || e.key === 'R')) {
        e.preventDefault();
        void refresh();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [refresh]);

  const filtered = useMemo(() => {
    const q = debouncedQuery.trim().toLowerCase();
    if (!q) return items;
    // 按原始转写 + 润色后文本匹配关键词，覆盖用户能想起的两种内容。
    return items.filter(
      s =>
        s.rawTranscript.toLowerCase().includes(q) ||
        s.finalText.toLowerCase().includes(q),
    );
  }, [items, debouncedQuery]);
  const item = useMemo(
    () => filtered.find(s => s.id === selectedId) || filtered[0],
    [filtered, selectedId],
  );

  const onClear = async () => {
    if (items.length === 0) return;
    if (!confirm(t('history.confirmClear', { count: items.length }))) return;
    setActionError(null);
    try {
      await clearHistory();
      setItems([]);
      setSelectedId(null);
    } catch (error) {
      console.error('[history] failed to clear history', error);
      setActionError(t('history.clearFailed', { err: errorMessage(error) }));
    }
  };

  const onDelete = async () => {
    if (!item) return;
    const deletedId = item.id;
    setActionError(null);
    try {
      await deleteHistoryEntry(deletedId);
      setItems(prev => prev.filter(s => s.id !== deletedId));
      setSelectedId(current => (current === deletedId ? null : current));
    } catch (error) {
      console.error('[history] failed to delete history entry', error);
      setActionError(t('history.deleteFailed', { err: errorMessage(error) }));
    }
  };

  const onCopy = async () => {
    if (!item) return;
    try {
      if (!navigator.clipboard?.writeText) {
        throw new Error('clipboard unavailable');
      }
      // 润色失败/未产出时 finalText 为空，回退到原文，避免「复制」按钮复制空字符串
      // 导致原文无法从 UI 取回（polish 失败时仍能拿到识别原文）。
      await navigator.clipboard.writeText(item.finalText.trim() ? item.finalText : item.rawTranscript);
      setActionError(null);
      setJustCopied(true);
      window.setTimeout(() => setJustCopied(false), 1500);
    } catch (error) {
      console.error('[history] failed to copy entry', error);
      setActionError(t('history.copyFailed', { err: errorMessage(error) }));
    }
  };

  // 原文（识别结果）单独复制：润色失败或用户只想要未润色文本时使用。
  const onCopyRaw = async () => {
    if (!item) return;
    try {
      if (!navigator.clipboard?.writeText) {
        throw new Error('clipboard unavailable');
      }
      await navigator.clipboard.writeText(item.rawTranscript);
      setActionError(null);
      setJustCopiedRaw(true);
      window.setTimeout(() => setJustCopiedRaw(false), 1500);
    } catch (error) {
      console.error('[history] failed to copy raw transcript', error);
      setActionError(t('history.copyFailed', { err: errorMessage(error) }));
    }
  };

  const onExportAudio = async () => {
    if (!item || !item.hasAudioRecording) return;
    try {
      // Wry/WebKit 中 data URL 的 <a download> 可能不触发保存对话框，后端直接调系统对话框
      if (isTauri) {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('export_audio_recording', { sessionId: item.id });
      } else {
        const dataUrl = await readAudioRecording(item.id);
        if (!dataUrl || dataUrl === 'data:audio/wav;base64,') throw new Error('empty recording');
        const a = document.createElement('a');
        a.href = dataUrl;
        a.download = `openless-recording-${item.id}.wav`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
      }
      setActionError(null);
    } catch (error) {
      console.error('[history] failed to export recording', error);
      const msg = errorMessage(error);
      if (isUserCancelled(msg)) {
        setActionError(null);
        return;
      }
      if (msg === 'recording export failed') {
        setActionError(t('history.exportError'));
        return;
      }
      // wav 已被 retention / 条数 cap 清理：把按钮隐藏，不显示错误（用户没干错事）。
      if (msg.includes('recording not found') || msg.includes('not found')) {
        markAudioMissing(item.id);
        return;
      }
      setActionError(t('history.exportFailed', { err: msg }));
    }
  };

  // 对一条「转录失败 / 没识别到语音」的历史用当前 ASR provider 重新转录（issue #613）。
  // 后端读 recordings/<id>.wav → 重转 → 原地回写该条 rawTranscript/finalText、清 errorCode，
  // 返回整条记录；前端据此局部刷新。失败保留 + 自动重试已让这些条目的录音留得住，这里给
  // 持久失败（重试也没救回来）一个手动重转入口。
  const onRetranscribe = async () => {
    if (!item || !item.hasAudioRecording) return;
    setRetranscribing(true);
    setActionError(null);
    try {
      const updated = await retranscribeRecording(item.id);
      setItems(prev => prev.map(s => (s.id === updated.id ? updated : s)));
    } catch (error) {
      console.error('[history] retranscribe failed', error);
      const msg = errorMessage(error);
      // wav 已被 retention / 条数 cap 清理：隐藏入口，不报错（用户没干错事）。
      if (msg.includes('recording not found') || msg.includes('not found')) {
        markAudioMissing(item.id);
        return;
      }
      setActionError(t('history.retranscribeFailed', { err: msg }));
    } finally {
      setRetranscribing(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <PageHeader
        kicker={t('history.kicker')}
        title={t('history.title')}
        desc={t('history.desc')}
        right={
          <div style={{ display: 'flex', gap: 8 }}>
            <Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()}>{t('common.refresh')}</Btn>
            <Btn icon="trash" variant="ghost" size="sm" onClick={onClear}>{t('common.clear')}</Btn>
          </div>
        }
      />
      <div style={{ display: 'grid', gridTemplateColumns: narrow ? '1fr' : '300px 1fr', gap: 14, flex: 1, minHeight: 0 }}>
        {( !narrow || !narrowDetailOpen) && (
        <Card padding={0} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
          <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)' }}>
            <div style={{
              display: 'flex', alignItems: 'center', gap: 6,
              padding: '6px 10px', fontSize: 12,
              border: '0.5px solid var(--ol-line-strong)', borderRadius: 8,
              background: 'var(--ol-surface-2)', color: 'var(--ol-ink-3)',
            }}>
              <Icon name="search" size={12} />
              <input
                ref={searchInputRef}
                type="search"
                value={query}
                onChange={e => setQuery(e.target.value)}
                placeholder={t('history.searchPlaceholder', { shortcut: searchShortcut })}
                aria-label={t('history.searchPlaceholder', { shortcut: searchShortcut })}
                style={{
                  flex: 1, minWidth: 0,
                  outline: 'none', border: 0, background: 'transparent',
                  fontSize: 12, color: 'var(--ol-ink-1)', fontFamily: 'inherit',
                }}
              />
            </div>
            <div style={{ marginTop: 6, fontSize: 11, color: 'var(--ol-ink-4)' }}>
              {t('history.summary', { total: items.length, shown: filtered.length })}
            </div>
          </div>
          <div className="ol-thinscroll" style={{ flex: 1, overflow: 'auto', padding: 6 }}>
            {actionError && (
              <div style={{ margin: 8, padding: '9px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 12, lineHeight: 1.45 }}>
                {actionError}
              </div>
            )}
            {loading && <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>}
            {!loading && loadError && (
              <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)', display: 'flex', flexDirection: 'column', alignItems: 'flex-start', gap: 10 }}>
                <span>{t('history.loadFailed', { err: loadError })}</span>
                <Btn size="sm" variant="ghost" onClick={() => void refresh()}>{t('history.retry')}</Btn>
              </div>
            )}
            {!loading && !loadError && filtered.length === 0 && (
              <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>
                {debouncedQuery.trim()
                  ? t('history.searchNoMatch', { query: debouncedQuery.trim() })
                  : t('history.empty', { trigger: prefs ? formatComboLabel(prefs.dictationHotkey) : '' })}
              </div>
            )}
            {!loadError && filtered.map(s => (
              <button
                key={s.id}
                onClick={() => {
                  setSelectedId(s.id);
                  if (narrow) setNarrowDetailOpen(true);
                }}
                style={{
                  width: '100%', padding: '10px 12px', textAlign: 'left',
                  display: 'flex', flexDirection: 'column', gap: 4,
                  border: 0, borderRadius: 8,
                  background: selectedId === s.id ? 'rgba(37,99,235,0.06)' : 'transparent',
                  boxShadow: selectedId === s.id ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
                  cursor: 'default', fontFamily: 'inherit', marginBottom: 1,
                  transition: 'background 0.16s var(--ol-motion-quick), box-shadow 0.18s var(--ol-motion-soft)',
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
                  <span style={{ fontSize: 11, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>
                    {formatTime(s.createdAt)}
                  </span>
                  <span style={{ fontSize: 10, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)' }}>
                    {formatDuration(s.durationMs, t)}
                  </span>
                </div>
                <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', lineHeight: 1.45, display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}>
                  {s.finalText.split('\n')[0]}
                </div>
              </button>
            ))}
          </div>
        </Card>
        )}

        {(!narrow || narrowDetailOpen) && (
        <Card padding={20} className="ol-thinscroll" style={{ overflow: 'auto' }}>
          {item ? (
            <>
              {narrow && (
                <div style={{ marginBottom: 12 }}>
                  <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => setNarrowDetailOpen(false)}>
                    {t('history.backToList')}
                  </Btn>
                </div>
              )}
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 14, flexWrap: 'wrap', gap: 8 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
                  <span style={{ fontSize: 13, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)', flexShrink: 0 }}>{formatTime(item.createdAt)}</span>
                  {/* 「录音」前缀：与下方识别/润色耗时区分——录音时长发生在松键前，
                      不该与流水线各步耗时加总（用户反馈"时间对不上"）。 */}
                  <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('history.recorded', { duration: formatDuration(item.durationMs, t) })}</span>
                </div>
                <div style={{ display: 'flex', gap: 6 }}>
                  {item.hasAudioRecording && !audioMissingIds.has(item.id) && (
                    <Btn icon="download" variant="ghost" size="sm" onClick={() => void onExportAudio()}>{t('history.exportRecording')}</Btn>
                  )}
                  {item.hasAudioRecording
                    && !audioMissingIds.has(item.id)
                    && (item.errorCode === 'transcribeFailed' || item.errorCode === 'emptyTranscript') && (
                    <Btn icon="refresh" variant="ghost" size="sm" disabled={retranscribing} onClick={() => void onRetranscribe()}>
                      {retranscribing ? t('history.retranscribing') : t('history.retranscribe')}
                    </Btn>
                  )}
                  <Btn icon="trash" variant="ghost" size="sm" onClick={onDelete}>{t('common.delete')}</Btn>
                </div>
              </div>
              {/* key 必须带组件前缀：下面的 RepolishPanel 是同一层的兄弟节点，两个都写
                  裸 `item.id` 会让同层出现重复 key，React 只警告不报错，但 reconcile 匹配
                  不上旧 fiber —— 每切换一次历史条目就在 DOM 里残留一个「播放录音」按钮，
                  开着不关的窗口能叠出一整列。 */}
              {item.hasAudioRecording && !audioMissingIds.has(item.id) && (
                <AudioRecordingPlayer
                  sessionId={item.id}
                  onMissing={() => markAudioMissing(item.id)}
                  key={`audio-${item.id}`}
                />
              )}
              {/* 流水线明细：识别 / 润色 / 插入 三步各占一行 —— 左列步骤名、中列
                  provider·model（或插入目标），右列该步耗时/状态。旧历史没有模型与
                  耗时字段时对应行自动隐藏，只剩插入行 = 改版前的信息量。 */}
              <div style={{ marginBottom: 16, paddingBottom: 14, borderBottom: '0.5px solid var(--ol-line-soft)', display: 'grid', gridTemplateColumns: 'auto 1fr auto', columnGap: 14, rowGap: 7, fontSize: 11, color: 'var(--ol-ink-4)', alignItems: 'baseline' }}>
                {(item.asrProvider || item.asrMs != null) && (
                  <>
                    <span style={{ display: 'flex' }}>
                      <Tooltip content={t('history.stepAsrHint')} wrap placement="bottom" focusable>
                        <span style={{ cursor: 'help', textDecoration: 'underline dotted', textDecorationColor: 'var(--ol-ink-4)', textUnderlineOffset: 3 }}>
                          {t('history.stepAsr')}
                        </span>
                      </Tooltip>
                    </span>
                    <span style={{ color: 'var(--ol-ink-2)', fontFamily: 'var(--ol-font-mono)', overflowWrap: 'anywhere' }}>
                      {[item.asrProvider, item.asrModel].filter(Boolean).join(' · ')}
                    </span>
                    <span style={{ fontFamily: 'var(--ol-font-mono)', textAlign: 'right', whiteSpace: 'nowrap' }}>
                      {item.asrMs != null ? formatStepDuration(item.asrMs, t) : ''}
                    </span>
                  </>
                )}
                {(item.llmProvider || item.llmModel || item.polishMs != null) && (
                  <>
                    <span>{t('history.stepPolish')}</span>
                    <span style={{ color: 'var(--ol-ink-2)', fontFamily: 'var(--ol-font-mono)', overflowWrap: 'anywhere' }}>
                      {[item.llmProvider, item.llmModel].filter(Boolean).join(' · ')}
                    </span>
                    <span style={{ fontFamily: 'var(--ol-font-mono)', textAlign: 'right', whiteSpace: 'nowrap' }}>
                      {item.polishMs != null ? formatStepDuration(item.polishMs, t) : ''}
                    </span>
                  </>
                )}
                <span>{t('history.stepInsert')}</span>
                <span style={{ color: 'var(--ol-ink-2)' }}>
                  {item.appName && <><b>{item.appName}</b>{' · '}</>}
                  {/* 按 Unicode 码点计（emoji / CJK 扩展 B 等增补平面字符不按 UTF-16 码元双算），
                      与后端 `polished.chars().count()` 及概览页「字数」口径一致。 */}
                  {t('history.chars', { count: countCodePoints(item.finalText) })}
                  {item.dictionaryEntryCount != null && item.dictionaryEntryCount > 0 && (
                    <>{' · '}{t('history.vocabHits', { count: item.dictionaryEntryCount })}</>
                  )}
                </span>
                <span style={{ textAlign: 'right', whiteSpace: 'nowrap' }}>{
                  item.insertStatus === 'inserted'
                    ? t('history.inserted')
                    : item.insertStatus === 'pasteSent'
                      ? t('history.pasteSent')
                    : item.insertStatus === 'copiedFallback'
                      ? t('history.copiedFallback', { shortcut: os === 'mac' ? '⌘V' : 'Ctrl+V' })
                      : t('history.insertFailed')
                }</span>
              </div>
              {/* Keep both text columns within the available width. */}
              <div style={{ display: 'grid', gridTemplateColumns: narrow ? '1fr' : '1fr 1fr', gap: 12 }}>
                <div style={{ minWidth: 0, padding: 14, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
                    <span>{t('history.rawLabel')}</span>
                    {item.rawTranscript && (
                      <Btn icon={justCopiedRaw ? 'check' : 'copy'} variant="ghost" size="sm" onClick={() => void onCopyRaw()}>
                        {justCopiedRaw ? t('common.copied') : t('common.copy')}
                      </Btn>
                    )}
                  </div>
                  <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink-2)', whiteSpace: 'pre-wrap' }}>
                    {item.rawTranscript || t('history.rawEmpty')}
                  </p>
                </div>
                <div style={{ minWidth: 0, padding: 14, border: '0.5px solid var(--ol-blue)', borderRadius: 10, background: 'var(--ol-blue-soft)' }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
                    <span>{t('history.resultLabel')}</span>
                    <span style={{ flexShrink: 0 }}>
                      <Btn icon={justCopied ? 'check' : 'copy'} variant="ghost" size="sm" onClick={() => void onCopy()}>
                        {justCopied ? t('common.copied') : t('common.copy')}
                      </Btn>
                    </span>
                  </div>
                  <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink)', whiteSpace: 'pre-line' }}>
                    {item.finalText}
                  </p>
                </div>
              </div>
              {/* 润色诊断：这次实际送入模型的完整 prompt（含 OCR/光标上下文）。
                  默认折叠；大文本只在展开后渲染。旧记录/重新转写后的条目没有快照。 */}
              <PolishDiagnosticsSection diagnostics={item.polishDiagnostics ?? null} />
              {/* 重新润色：拿这条的原文再跑一次 LLM。没有原文就不渲染。
                  key 让切换记录时结果与状态一起重置，避免把上一条的结果留在新条目下面；
                  前缀是为了跟上面播放器的 key 区分开（同层重复 key 会残留旧节点）。 */}
              {item.rawTranscript.trim() && (
                <RepolishPanel
                  session={item}
                  narrow={narrow}
                  key={`repolish-${item.id}`}
                />
              )}
            </>
          ) : (
            <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
              {loading ? t('common.loading') : loadError ? t('history.loadFailed', { err: loadError }) : t('history.selectHint')}
            </div>
          )}
        </Card>
        )}
      </div>
    </div>
  );
}

const DIAG_PRE_STYLE = {
  margin: 0,
  padding: 12,
  fontSize: 11.5,
  lineHeight: 1.6,
  fontFamily: 'var(--ol-font-mono)',
  color: 'var(--ol-ink-2)',
  background: 'var(--ol-surface-2)',
  border: '0.5px solid var(--ol-line)',
  borderRadius: 8,
  whiteSpace: 'pre-wrap',
  overflowWrap: 'anywhere',
  maxHeight: 360,
  overflow: 'auto',
} as const;

function diagnosticsRow(label: string, value: ReactNode) {
  return (
    <div style={{ display: 'grid', gridTemplateColumns: '84px 1fr', gap: 8, alignItems: 'baseline' }}>
      <span style={{ color: 'var(--ol-ink-4)' }}>{label}</span>
      <span style={{ color: 'var(--ol-ink-2)', fontFamily: 'var(--ol-font-mono)', overflowWrap: 'anywhere' }}>
        {value}
      </span>
    </div>
  );
}

function ContextDiagnosticsRow({ label, context }: { label: string; context: PolishContextDiagnostics | null }) {
  const { t } = useTranslation();
  const summary = contextSummary(context);
  return (
    <div style={{ display: 'grid', gridTemplateColumns: '84px 1fr', gap: 8, alignItems: 'baseline' }}>
      <span style={{ color: 'var(--ol-ink-4)' }}>{label}</span>
      <span style={{ color: 'var(--ol-ink-2)', overflowWrap: 'anywhere' }}>
        {t(`history.diagnostics.status.${summary.statusKey}`)}
        {summary.reason ? <span style={{ color: 'var(--ol-ink-4)' }}>{` · ${summary.reason}`}</span> : null}
        {summary.hasText ? (
          <span style={{ color: 'var(--ol-ink-4)' }}>{` · ${t('history.diagnostics.contextTextChars', { count: summary.textChars })}`}</span>
        ) : null}
      </span>
    </div>
  );
}

/**
 * 「润色诊断」折叠区：诊断快照来自后端在真正渲染 prompt 处的捕获（不是前端拼的）。
 * 隐私边界：完整 prompt 只在本机 UI 展示与复制；默认折叠、大文本按需渲染。
 * null = 未保存（旧版本记录、链路未提供快照、或重新转写后已清除）。
 */
function PolishDiagnosticsSection({ diagnostics }: { diagnostics: PolishDiagnosticsValue | null }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState<'prompt' | 'all' | null>(null);
  const [copyError, setCopyError] = useState<string | null>(null);

  const onCopy = async (what: 'prompt' | 'all') => {
    if (!diagnostics) return;
    try {
      if (!navigator.clipboard?.writeText) throw new Error('clipboard unavailable');
      const text = what === 'prompt' ? diagnostics.prompt ?? '' : formatPolishDiagnosticsJson(diagnostics);
      await navigator.clipboard.writeText(text);
      setCopyError(null);
      setCopied(what);
      window.setTimeout(() => setCopied(current => (current === what ? null : current)), 1500);
    } catch (error) {
      console.error('[history] failed to copy polish diagnostics', error);
      setCopyError(t('history.copyFailed', { err: String(error) }));
    }
  };

  if (!diagnostics) {
    return (
      <div style={{ marginTop: 14, paddingTop: 12, borderTop: '0.5px solid var(--ol-line-soft)', fontSize: 11, color: 'var(--ol-ink-4)' }}>
        <span style={{ fontWeight: 600, color: 'var(--ol-ink-3)' }}>{t('history.diagnostics.title')}</span>
        {' · '}
        {t('history.diagnostics.notSaved')}
      </div>
    );
  }

  const headline = diagnosticsHeadlineParts(diagnostics).join(' · ');
  return (
    <details style={{ marginTop: 14, paddingTop: 12, borderTop: '0.5px solid var(--ol-line-soft)' }}>
      <summary style={{ cursor: 'default', fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)', display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
        <span>{t('history.diagnostics.title')}</span>
        <Pill size="sm" tone={diagnostics.outcome === 'succeeded' ? 'blue' : diagnostics.outcome === 'failed' ? 'outline' : 'default'} style={TRUNCATED_PILL_STYLE}>
          {t(`history.diagnostics.outcome.${outcomeLabelKey(diagnostics.outcome)}`)}
        </Pill>
        <span style={{ fontSize: 11, fontWeight: 400, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)' }}>{headline}</span>
      </summary>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 7, marginTop: 12, fontSize: 11, lineHeight: 1.55 }}>
        {diagnosticsRow(t('history.diagnostics.stageLabel'), t(`history.diagnostics.stage.${stageLabelKey(diagnostics.stage)}`))}
        {diagnosticsRow(t('history.diagnostics.model'), [diagnostics.provider, diagnostics.model].filter(Boolean).join(' · ') || '—')}
        {diagnostics.template
          ? diagnosticsRow(t('history.diagnostics.templateLabel'), t(`history.diagnostics.template.${diagnostics.template}`))
          : null}
        {diagnostics.addBos != null
          ? diagnosticsRow(t('history.diagnostics.addBos'), t(`history.diagnostics.addBosValue.${diagnostics.addBos ? 'injected' : 'off'}`))
          : null}
        {diagnostics.promptTokens != null || diagnostics.generationBudget != null
          ? diagnosticsRow(
              t('history.diagnostics.tokens'),
              t('history.diagnostics.tokenLine', {
                prompt: diagnostics.promptTokens ?? '—',
                ctx: diagnostics.contextTokens ?? '—',
                budget: diagnostics.generationBudget ?? '—',
              }),
            )
          : null}
        <ContextDiagnosticsRow label={t('history.diagnostics.contextCursor')} context={diagnostics.cursor} />
        <ContextDiagnosticsRow label={t('history.diagnostics.contextOcr')} context={diagnostics.screenOcr} />
        {diagnostics.notCalledReason
          ? diagnosticsRow(t('history.diagnostics.notCalledReason'), diagnostics.notCalledReason)
          : null}
        {diagnostics.error
          ? diagnosticsRow(
              t('history.diagnostics.error'),
              `${diagnostics.error.code}: ${diagnostics.error.message}`,
            )
          : null}
      </div>

      {diagnostics.prompt != null && (
        <div style={{ marginTop: 12 }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 6 }}>
            <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('history.diagnostics.promptSection')}</span>
            <Btn
              icon={copied === 'prompt' ? 'check' : 'copy'}
              variant="ghost"
              size="sm"
              onClick={() => void onCopy('prompt')}
            >
              {copied === 'prompt' ? t('common.copied') : t('history.diagnostics.copyPrompt')}
            </Btn>
          </div>
          {/* 纯文本 <pre>：prompt 是数据，绝不用 HTML 渲染。 */}
          <pre className="ol-thinscroll" style={DIAG_PRE_STYLE}>{diagnostics.prompt}</pre>
        </div>
      )}

      {diagnostics.messages != null && diagnostics.messages.length > 0 && (
        <details style={{ marginTop: 12 }}>
          <summary style={{ cursor: 'default', fontSize: 11, color: 'var(--ol-ink-4)' }}>
            {t('history.diagnostics.messagesSection', { count: diagnostics.messages.length })}
          </summary>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 8 }}>
            {diagnostics.messages.map((message, index) => (
              <details key={`${message.role}-${index}`} style={{ border: '0.5px solid var(--ol-line)', borderRadius: 8, background: 'var(--ol-surface-2)' }}>
                <summary style={{ cursor: 'default', padding: '6px 10px', fontSize: 11, color: 'var(--ol-ink-3)', fontFamily: 'var(--ol-font-mono)' }}>
                  {`#${index} ${message.role} · ${message.content.length} chars`}
                </summary>
                <pre className="ol-thinscroll" style={{ ...DIAG_PRE_STYLE, border: 0, borderRadius: '0 0 8px 8px' }}>
                  {JSON.stringify(message, null, 2)}
                </pre>
              </details>
            ))}
          </div>
        </details>
      )}

      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 12, flexWrap: 'wrap' }}>
        <Btn
          icon={copied === 'all' ? 'check' : 'copy'}
          variant="ghost"
          size="sm"
          onClick={() => void onCopy('all')}
        >
          {copied === 'all' ? t('common.copied') : t('history.diagnostics.copyAll')}
        </Btn>
        {copyError && <span style={{ fontSize: 11, color: 'var(--ol-red, #ef4444)' }}>{copyError}</span>}
      </div>
    </details>
  );
}

/** 后端超时错误在 IPC 边界退化成裸字符串（LLMError::Timeout → "timeout"）。
 *  只匹配整串的常见超时形态，避免其它含 "timeout" 字样的错误被误判成超时。 */
function isTimeout(message: string): boolean {
  const trimmed = message.trim();
  return /^(timeout|timed out|request timed out)$/i.test(trimmed) || trimmed.includes('超时');
}

function errorMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

interface RepolishResult {
  /** 结果卡片的 key。重复整理会覆盖上一次，不无限堆卡片。 */
  key: string;
  title: string;
  text: string;
}

function RepolishPanel({ session, narrow }: {
  session: DictationSession;
  narrow: boolean;
}) {
  const { t } = useTranslation();
  const [running, setRunning] = useState<'retry' | 'apply' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [results, setResults] = useState<RepolishResult[]>([]);

  const run = async (kind: 'retry' | 'apply') => {
    setRunning(kind);
    setError(null);
    try {
      const text = await repolish(session.rawTranscript);
      const result: RepolishResult = {
        key: '__retry__',
        title: t('history.resultLabel'),
        text,
      };
      // 同一个 key 覆盖旧结果，新 key 追加到最前面 —— 最新的试算结果离操作区最近。
      setResults(prev => [result, ...prev.filter(r => r.key !== result.key)]);
    } catch (err) {
      console.error('[history] repolish failed', err);
      const msg = errorMessage(err);
      setError(
        isTimeout(msg)
          ? t('history.repolish.timeout')
          : t('history.repolish.failed', { err: msg }),
      );
    } finally {
      setRunning(null);
    }
  };

  return (
    <div style={{ marginTop: 18, paddingTop: 14, borderTop: '0.5px solid var(--ol-line-soft)' }}>
      <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 10, flexWrap: 'wrap', marginBottom: 4 }}>
        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)' }}>
          {t('history.repolish.title')}
        </span>
        {results.length > 0 && (
          <Btn size="sm" variant="ghost" onClick={() => setResults([])}>
            {t('history.repolish.clear')}
          </Btn>
        )}
      </div>
      <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.55, marginBottom: 12 }}>
        {t('history.repolish.hint')}
      </div>

      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginBottom: results.length > 0 ? 14 : 0 }}>
        <Btn
          icon="refresh"
          variant="ghost"
          size="sm"
          disabled={running !== null}
          onClick={() => void run('retry')}
        >
          {running === 'retry' ? t('history.repolish.retrying') : t('history.repolish.title')}
        </Btn>
      </div>

      {error && (
        <div style={{ marginTop: 10, padding: '8px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 11.5, lineHeight: 1.45 }}>
          {error}
        </div>
      )}

      {results.length > 0 && (
        <div style={{ display: 'grid', gridTemplateColumns: narrow ? '1fr' : '1fr 1fr', gap: 12 }}>
          {results.map(result => (
            <RepolishResultCard key={result.key} title={result.title} text={result.text} />
          ))}
        </div>
      )}
    </div>
  );
}

function RepolishResultCard({ title, text }: { title: string; text: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  const onCopy = async () => {
    try {
      if (!navigator.clipboard?.writeText) throw new Error('clipboard unavailable');
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      console.error('[history] failed to copy repolish result', error);
    }
  };

  return (
    // minWidth: 0 —— grid 子项默认 min-width: auto，标题 Pill 不换行时会把卡片
    // 撑出结果网格（与详情页两栏文本卡片同一类问题）。
    <div style={{ minWidth: 0, padding: 14, border: '0.5px dashed var(--ol-line-strong)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
        <span style={{ display: 'flex', minWidth: 0 }} title={title}>
          <span>{title}</span>
        </span>
        {text.trim() && (
          <Btn icon={copied ? 'check' : 'copy'} variant="ghost" size="sm" onClick={() => void onCopy()}>
            {copied ? t('common.copied') : t('common.copy')}
          </Btn>
        )}
      </div>
      <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink-2)', whiteSpace: 'pre-wrap' }}>
        {text.trim() || t('history.repolish.empty')}
      </p>
    </div>
  );
}

function isUserCancelled(message: string): boolean {
  const normalized = message.trim().toLowerCase();
  return normalized === 'cancelled'
    || normalized === 'canceled'
    || normalized === 'user cancelled'
    || normalized === 'user canceled';
}

/** 当 session.hasAudioRecording 为 true 时渲染：一个加载按钮 + 拿到字节后切换为
 *  原生 audio controls。Blob URL 在组件 unmount 时 revoke，避免泄漏。
 *  `onMissing` 在后端返回 'recording not found'（wav 已被 prune）时触发，让父组件
 *  把按钮永久隐藏，避免用户继续点击得到同样错误。 */
function AudioRecordingPlayer({
  sessionId,
  onMissing,
}: {
  sessionId: string;
  onMissing?: () => void;
}) {
  const { t } = useTranslation();
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'loading' | 'ready' | 'error'>('idle');
  const [errorText, setErrorText] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const blobUrlRef = useRef<string | null>(null);

  // 组件 unmount 时释放 Blob URL，避免内存泄漏。
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (blobUrlRef.current) {
        URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = null;
      }
    };
  }, []);

  const clearBlobUrl = () => {
    if (blobUrlRef.current) {
      URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = null;
    }
    setBlobUrl(null);
  };

  const load = async () => {
    setStatus('loading');
    setErrorText(null);
    try {
      const dataUrl = await readAudioRecording(sessionId);
      if (!mountedRef.current) return;
      if (!dataUrl || dataUrl === 'data:audio/wav;base64,') throw new Error('empty recording');
      // WebKitGTK <audio> 对 data: URL 解码不稳定（时长 0 / 播不动），
      // 把 base64 解码为二进制再封装成 Blob URL，在 WebKit 里远更可靠。
      const comma = dataUrl.indexOf(',');
      const b64 = comma >= 0 ? dataUrl.slice(comma + 1) : '';
      if (!b64) throw new Error('empty recording');
      const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      const blob = new Blob([bin], { type: 'audio/wav' });
      const url = URL.createObjectURL(blob);
      if (!mountedRef.current) {
        URL.revokeObjectURL(url);
        return;
      }
      if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = url;
      setBlobUrl(url);
      setStatus('ready');
    } catch (error) {
      if (!mountedRef.current) return;
      console.error('[history] load recording failed', error);
      const msg = errorMessage(error);
      if (msg.includes('recording not found') || msg.includes('not found')) {
        onMissing?.();
        return;
      }
      setStatus('error');
      setErrorText(msg);
    }
  };

  if (status === 'ready' && blobUrl) {
    return (
      <div style={{ marginBottom: 14 }}>
        <audio
          src={blobUrl}
          controls
          preload="auto"
          autoPlay
          style={{ width: '100%' }}
          onError={(e) => {
            if (!mountedRef.current) return;
            const a = e.currentTarget;
            const code = a.error?.code ?? -1;
            const detail = a.error?.message ?? `${code}`;
            console.error('[history] <audio> decode/play failed', { code, detail });
            clearBlobUrl();
            setStatus('error');
            setErrorText(t('history.audioDecodeFailed', { err: detail }));
          }}
        />
      </div>
    );
  }
  return (
    <div style={{ marginBottom: 14, display: 'flex', alignItems: 'center', gap: 10 }}>
      <Btn
        icon="play"
        variant="ghost"
        size="sm"
        onClick={() => void load()}
        disabled={status === 'loading'}
      >
        {status === 'loading' ? t('history.audioLoading') : t('history.playRecording')}
      </Btn>
      {status === 'error' && (
        <span style={{ fontSize: 11, color: 'var(--ol-err)' }}>{errorText}</span>
      )}
    </div>
  );
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const pad = (n: number) => String(n).padStart(2, '0');
  if (sameDay) return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 流水线单步耗时：<1s 显示整数毫秒（流式收尾常在几十 ms，0.1s 精度会把不同结果
 *  拍成同一个值，模型对比就失真了——PR #826 review）；≥1s 沿用 0.1s 精度。 */
function formatStepDuration(ms: number, t: ReturnType<typeof useTranslation>['t']): string {
  if (ms < 1000) return t('common.durationMillis', { value: Math.round(ms) });
  return formatDuration(ms, t);
}

function formatDuration(ms: number | null, t: ReturnType<typeof useTranslation>['t']): string {
  if (ms == null || ms <= 0) return '—';
  const sec = ms / 1000;
  if (sec < 60) return t('common.durationSeconds', { value: sec.toFixed(1) });
  return t('common.durationMinutes', { value: (sec / 60).toFixed(1) });
}
