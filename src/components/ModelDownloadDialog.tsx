// ModelDownloadDialog.tsx — 首启模型下载引导对话框。
// 只在用户显式点击「获取模型」后出现并开始下载；下载源与校验钉死在
// Rust 侧 manifest（pinned revision + SHA-256），这里只提供 ASR 精度
// 二选一、镜像选择与真实进度展示。下载完成后 Rust 侧自动重预载引擎，
// 概览卡状态经既有 engine-changed 事件刷新，本组件不做假状态。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Btn } from '../pages/_atoms';
import {
  cancelModelDownload,
  listenModelDownloadProgress,
  startModelDownload,
  type ModelDownloadKey,
  type ModelDownloadMirror,
} from '../lib/ipc/modelDownload';
import {
  applyDownloadProgress,
  downloadRowPercent,
  formatDownloadBytes,
  idleDownloadRow,
  type DownloadRowState,
} from '../lib/modelDownloadState';

interface ModelDownloadDialogProps {
  open: boolean;
  onClose: () => void;
  missingAsr: boolean;
  missingLlm: boolean;
}

const ASR_CHOICES: Array<{ key: ModelDownloadKey; labelKey: string }> = [
  { key: 'asr-bf16', labelKey: 'overview.downloadPrecisionFull' },
  { key: 'asr-4bit', labelKey: 'overview.downloadPrecision4bit' },
];

export function ModelDownloadDialog({ open, onClose, missingAsr, missingLlm }: ModelDownloadDialogProps) {
  const { t } = useTranslation();
  const [asrKey, setAsrKey] = useState<ModelDownloadKey>('asr-bf16');
  const [mirror, setMirror] = useState<ModelDownloadMirror>('huggingface');
  const [rows, setRows] = useState<Record<string, DownloadRowState>>({});
  const rowsRef = useRef(rows);
  rowsRef.current = rows;

  // 终态行集合，用于启动后标记「已启动过」的行。
  const [startedKeys, setStartedKeys] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!open) return;
    let disposed = false;
    let stop: (() => void) | null = null;
    void listenModelDownloadProgress((progress) => {
      if (disposed) return;
      setRows((current) => ({
        ...current,
        [progress.key]: applyDownloadProgress(
          current[progress.key] ?? idleDownloadRow(),
          progress,
        ),
      }));
    }).then((unlisten) => {
      if (disposed) unlisten();
      else stop = unlisten;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, [open]);

  const anyDownloading = Object.values(rows).some((row) => row.phase === 'downloading');

  const start = useCallback(
    (key: ModelDownloadKey) => {
      setRows((current) => ({ ...current, [key]: idleDownloadRow() }));
      setStartedKeys((current) => new Set(current).add(key));
      void startModelDownload(key, mirror).catch(() => {
        // 命令层拒绝（如重复启动）：Failed 事件会跟进；这里只清本地启动态。
        setRows((current) => ({
          ...current,
          [key]: { ...idleDownloadRow(), phase: 'failed', error: 'start failed' },
        }));
      });
    },
    [mirror],
  );

  const cancel = useCallback((key: ModelDownloadKey) => {
    void cancelModelDownload(key).catch(() => undefined);
  }, []);

  if (!open) return null;

  const visibleRows: Array<{ key: ModelDownloadKey; name: string }> = [];
  if (missingAsr) visibleRows.push({ key: asrKey, name: 'Qwen3-ASR 1.7B' });
  if (missingLlm) visibleRows.push({ key: 'llm-gguf', name: 'Gemma 4 E4B' });

  const rowView = (key: ModelDownloadKey, name: string) => {
    const row = rows[key] ?? idleDownloadRow();
    const downloading = row.phase === 'downloading';
    const percent = downloadRowPercent(row);
    return (
      <div
        key={key}
        style={{
          border: '1px solid var(--ol-line, rgba(0,0,0,0.08))',
          borderRadius: 10,
          padding: '12px 14px',
          marginBottom: 10,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
          <div style={{ fontSize: 13, fontWeight: 600, color: 'var(--ol-ink)' }}>{name}</div>
          {row.phase === 'finished' ? (
            <span style={{ fontSize: 12, color: 'var(--ol-ok, #16a34a)' }}>{t('overview.downloadPhaseFinished')}</span>
          ) : row.phase === 'failed' ? (
            <span style={{ fontSize: 12, color: 'var(--ol-red, #ef4444)' }}>
              {t('overview.downloadPhaseFailed')}
            </span>
          ) : row.phase === 'cancelled' ? (
            <span style={{ fontSize: 12, color: 'var(--ol-ink-3)' }}>{t('overview.downloadPhaseCancelled')}</span>
          ) : null}
        </div>
        {key === asrKey && missingAsr && !startedKeys.has(key) && (
          <div style={{ marginTop: 8, display: 'grid', gap: 6 }}>
            {ASR_CHOICES.map((choice) => (
              <label
                key={choice.key}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 8,
                  fontSize: 12.5,
                  color: 'var(--ol-ink-2)',
                  cursor: 'pointer',
                }}
              >
                <input
                  type="radio"
                  name="asr-precision"
                  checked={asrKey === choice.key}
                  onChange={() => setAsrKey(choice.key)}
                />
                {t(choice.labelKey)}
              </label>
            ))}
          </div>
        )}
        {downloading && (
          <div style={{ marginTop: 8 }}>
            <div
              style={{
                height: 6,
                borderRadius: 999,
                background: 'var(--ol-blue-soft, rgba(59,130,246,0.12))',
                overflow: 'hidden',
              }}
            >
              <div
                style={{
                  height: '100%',
                  width: `${percent ?? 0}%`,
                  background: 'var(--ol-blue)',
                  transition: 'width .3s ease',
                }}
              />
            </div>
            <div
              style={{
                marginTop: 4,
                fontSize: 11,
                color: 'var(--ol-ink-3)',
                fontFamily: 'var(--ol-font-mono)',
                display: 'flex',
                justifyContent: 'space-between',
                gap: 8,
              }}
            >
              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {row.file}
              </span>
              <span style={{ flexShrink: 0 }}>
                {formatDownloadBytes(row.bytes)} / {formatDownloadBytes(row.total)}
                {percent != null ? ` (${percent}%)` : ''}
              </span>
            </div>
          </div>
        )}
        {row.phase === 'failed' && row.error && (
          <div
            title={row.error}
            style={{
              marginTop: 8,
              fontSize: 11.5,
              color: 'var(--ol-red, #ef4444)',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
            }}
          >
            {row.error}
          </div>
        )}
        <div style={{ marginTop: 10, display: 'flex', gap: 8 }}>
          {!startedKeys.has(key) && (
            <Btn onClick={() => start(key)}>{t('overview.downloadStart')}</Btn>
          )}
          {downloading && (
            <Btn onClick={() => cancel(key)}>{t('overview.downloadCancel')}</Btn>
          )}
          {row.phase === 'cancelled' && (
            <Btn onClick={() => start(key)}>{t('overview.downloadStart')}</Btn>
          )}
          {row.phase === 'failed' && (
            <Btn onClick={() => start(key)}>{t('overview.downloadStart')}</Btn>
          )}
        </div>
      </div>
    );
  };

  return (
    <div
      role="dialog"
      aria-modal
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 1000,
        background: 'rgba(0,0,0,0.35)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 20,
      }}
      onClick={(event) => {
        // 下载进行中禁止点背景关闭，防误触中断感知（取消有专门按钮）。
        if (event.target === event.currentTarget && !anyDownloading) onClose();
      }}
    >
      <div
        style={{
          width: 520,
          maxWidth: '100%',
          maxHeight: '80vh',
          overflowY: 'auto',
          background: 'var(--ol-bg, #fff)',
          borderRadius: 14,
          padding: 20,
          boxShadow: '0 18px 50px rgba(0,0,0,0.22)',
        }}
      >
        <div style={{ fontSize: 15, fontWeight: 700, color: 'var(--ol-ink)', marginBottom: 6 }}>
          {t('overview.downloadTitle')}
        </div>
        <div style={{ fontSize: 12.5, color: 'var(--ol-ink-3)', marginBottom: 14, lineHeight: 1.5 }}>
          {t('overview.downloadIntro')}
        </div>
        {visibleRows.map(({ key, name }) => rowView(key, name))}
        <div style={{ marginTop: 6, marginBottom: 14 }}>
          <div style={{ fontSize: 12, color: 'var(--ol-ink-3)', marginBottom: 4 }}>
            {t('overview.downloadMirror')}
          </div>
          <select
            value={mirror}
            onChange={(event) => setMirror(event.target.value as ModelDownloadMirror)}
            disabled={anyDownloading}
            style={{
              fontSize: 12.5,
              padding: '6px 8px',
              borderRadius: 8,
              border: '1px solid var(--ol-line, rgba(0,0,0,0.12))',
              background: 'var(--ol-bg, #fff)',
              color: 'var(--ol-ink)',
            }}
          >
            <option value="huggingface">{t('overview.downloadMirrorOfficial')}</option>
            <option value="hf-mirror">{t('overview.downloadMirrorCn')}</option>
          </select>
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
          <Btn onClick={onClose} disabled={anyDownloading}>
            {t('overview.downloadClose')}
          </Btn>
        </div>
      </div>
    </div>
  );
}
