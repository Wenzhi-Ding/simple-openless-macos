// Vocab.tsx — 接 Tauri 后端 list_vocab / add_vocab / remove_vocab / set_vocab_enabled。
// 数据落地到 ~/Library/Application Support/OpenLess/dictionary.json（与 Swift 同名）。

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { addVocab, isTauri, listVocab, removeVocab, setVocabEnabled } from '../lib/ipc';
import type { DictionaryEntry } from '../lib/types';
import { Btn, Card, Collapsible, PageHeader } from './_atoms';

export function Vocab() {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<DictionaryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const inputRef = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      setError(null);
      setEntries(await listVocab());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const handle = await listen('vocab:updated', () => void refresh());
      if (cancelled) handle();
      else unlisten = handle;
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const onAdd = async () => {
    const phrase = inputRef.current?.value.trim();
    if (!phrase) return;
    try {
      await addVocab(phrase);
      if (inputRef.current) inputRef.current.value = '';
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const onRemove = async (id: string) => {
    try {
      await removeVocab(id);
      setEntries(prev => prev.filter(entry => entry.id !== id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const onToggle = async (entry: DictionaryEntry) => {
    const enabled = !entry.enabled;
    setEntries(prev => prev.map(item => item.id === entry.id ? { ...item, enabled } : item));
    try {
      await setVocabEnabled(entry.id, enabled);
    } catch (e) {
      setEntries(prev => prev.map(item => item.id === entry.id ? entry : item));
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <>
      <PageHeader
        kicker={t('vocab.kicker')}
        title={t('vocab.title')}
        desc={t('vocab.desc')}
        right={<Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()}>{t('common.refresh')}</Btn>}
      />
      <Card padding={0}>
        <Collapsible embedded defaultOpen title={t('vocab.sectionTitle')} desc={t('vocab.tip')}>
          <div style={{ display: 'flex', gap: 8 }}>
            <input
              ref={inputRef}
              placeholder={t('vocab.placeholder')}
              onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); void onAdd(); } }}
              style={{ flex: 1, height: 36, padding: '0 12px', border: '0.5px solid var(--ol-line-strong)', borderRadius: 8, fontSize: 13, fontFamily: 'inherit', outline: 'none', background: 'var(--ol-surface-2)' }}
            />
            <Btn variant="primary" icon="plus" onClick={() => void onAdd()}>{t('common.add')}</Btn>
          </div>
          <div style={{ marginTop: 12, display: 'flex', flexWrap: 'wrap', gap: 8, minHeight: 80 }}>
            {loading && <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>}
            {!loading && error && <div style={{ fontSize: 12, color: 'var(--ol-err)', lineHeight: 1.6 }}>{t('vocab.loadFailed', { err: error })}</div>}
            {!loading && !error && entries.length === 0 && <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('vocab.empty')}</div>}
            {!error && entries.map(entry => <VocabChip key={entry.id} entry={entry} onRemove={() => void onRemove(entry.id)} onToggle={() => void onToggle(entry)} />)}
          </div>
        </Collapsible>
      </Card>
      <style>{`@keyframes ol-chip-in { from { opacity: 0; transform: scale(.92); filter: blur(5px); } to { opacity: 1; transform: scale(1); filter: blur(0); } }`}</style>
    </>
  );
}

function VocabChip({ entry, onRemove, onToggle }: { entry: DictionaryEntry; onRemove: () => void; onToggle: () => void }) {
  const { t } = useTranslation();
  return (
    <span style={{ alignSelf: 'flex-start', minWidth: 0, maxWidth: '100%', boxSizing: 'border-box', display: 'inline-flex', alignItems: 'center', gap: 6, padding: '5px 10px 5px 12px', borderRadius: 999, border: '0.5px solid var(--ol-line-strong)', background: entry.enabled ? (entry.hits > 0 ? 'var(--ol-blue-soft)' : 'var(--ol-surface)') : 'var(--ol-surface-2)', opacity: entry.enabled ? 1 : 0.55, fontSize: 12, color: 'var(--ol-ink)', fontFamily: 'var(--ol-font-mono)', animation: 'ol-chip-in 0.22s var(--ol-motion-spring)' }}>
      <button onClick={onToggle} title={entry.enabled ? t('vocab.tipDisabled') : t('vocab.tipEnabled')} style={{ minWidth: 0, flex: '1 1 auto', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textAlign: 'left', background: 'transparent', border: 0, padding: 0, color: 'inherit', fontFamily: 'inherit', cursor: 'default' }}>{entry.phrase}</button>
      <span style={{ minWidth: 18, height: 18, padding: '0 5px', borderRadius: 999, fontSize: 10, fontWeight: 600, display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0, background: entry.hits > 0 && entry.enabled ? 'var(--ol-blue)' : 'rgba(0,0,0,0.06)', color: entry.hits > 0 && entry.enabled ? '#fff' : 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-sans)' }}>{entry.hits}</span>
      <button onClick={onRemove} aria-label={t('vocab.removeAria')} style={{ width: 14, height: 14, padding: 0, border: 0, borderRadius: 999, background: 'transparent', color: 'var(--ol-ink-4)', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0, cursor: 'default' }}><svg width="8" height="8" viewBox="0 0 8 8"><path d="M1 1l6 6M7 1l-6 6" stroke="currentColor" strokeWidth="1.4" /></svg></button>
    </span>
  );
}
