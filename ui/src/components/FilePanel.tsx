import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ipc, useMockIpc } from '../ipc/ipc';
import { EV_OS_DRAG_DROP, EV_OS_DRAG_ENTER, EV_OS_DRAG_LEAVE, pickImportFiles } from '../ipc/real';
import type { OsDragDropPayload } from '../ipc/real';
import type { ImportResult, PluginMatch } from '../ipc/types';
import { confidencePercent, formatBytes } from '../lib/format';
import { useSession } from '../state/session';
import './FilePanel.css';

function matchedPluginName(pluginId: string | null, plugins: { id: string; display_name: string }[]): string {
  if (!pluginId) return '—';
  return plugins.find((p) => p.id === pluginId)?.display_name ?? pluginId;
}

function FileEntry({
  entry,
  disabled,
  progressPercent,
  progressRecords,
}: {
  entry: ImportResult;
  disabled: boolean;
  progressPercent?: number;
  progressRecords?: number;
}) {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  /** 取消请求在途（按钮转「正在取消」禁用态；命令失败则复位）。 */
  const [cancelling, setCancelling] = useState(false);

  // 终态（error/ready/移除）后复位在途标记，避免重试后按钮残留禁用态。
  useEffect(() => {
    if (entry.status !== 'parsing') setCancelling(false);
  }, [entry.status]);

  const toggleDisabled = () => {
    if (!disabled) {
      const ids = state.metricTree
        .filter((n) => n.file_id === entry.file_id)
        .flatMap((n) => n.children ?? [])
        .flatMap((p) => p.children ?? [])
        .map((m) => m.id);
      if (ids.length > 0) actions.toggleMetrics(ids, false);
      actions.setFileDisabled(entry.file_id, true);
    } else {
      actions.setFileDisabled(entry.file_id, false);
    }
  };

  const switchPlugin = (pluginId: string) => {
    void actions.importFiles([entry.path], { [entry.path]: { plugin_id: pluginId } });
  };

  const pluginName = matchedPluginName(entry.matched_plugin?.plugin_id ?? null, state.plugins);
  const showChoice = entry.status === 'matched' && entry.needs_user_choice;

  return (
    <li className={`file-entry${disabled ? ' file-entry--disabled' : ''}`} data-testid="file-entry" data-file-id={entry.file_id}>
      <div className="file-entry__head">
        <div className="file-entry__name" title={entry.path}>
          {entry.name}
        </div>
        <span className={`file-entry__badge file-entry__badge--${entry.status}`} data-testid="status-badge">
          {entry.status === 'parsing'
            ? progressPercent !== undefined
              ? t('workbench.files.status_parsing_percent', { percent: progressPercent })
              : progressRecords !== undefined
                ? t('workbench.files.status_parsing', { records: progressRecords })
                : t('workbench.files.status_parsing_generic')
            : t(`workbench.files.status_${entry.status}`)}
        </span>
      </div>

      <div className="file-entry__meta">
        <span>{formatBytes(entry.size_bytes)}</span>
        <span className="file-entry__plugin">
          {showChoice
            ? t('workbench.files.choose_plugin_title')
            : entry.matched_plugin
              ? t('workbench.files.matched_plugin', {
                  name: pluginName,
                  confidence: confidencePercent(entry.matched_plugin.confidence),
                })
              : t('workbench.files.status_matched')}
        </span>
      </div>

      {entry.status === 'parsing' && (
        <div className="file-entry__progress" data-testid="progress">
          {progressPercent !== undefined ? (
            <div className="file-entry__progress-track">
              <div className="file-entry__progress-bar" style={{ width: `${progressPercent}%` }} />
            </div>
          ) : (
            <div className="file-entry__progress-indeterminate" />
          )}
          {progressPercent === undefined && progressRecords !== undefined && (
            <span className="file-entry__progress-records">
              {t('workbench.files.status_parsing', { records: progressRecords })}
            </span>
          )}
        </div>
      )}

      {entry.status === 'error' && entry.error && (
        <div className="file-entry__error" data-testid="entry-error">
          {t(`common.error.${entry.error.code}`, { defaultValue: entry.error.message })}
        </div>
      )}

      {entry.status === 'parsing' && (
        <button
          type="button"
          className="file-entry__btn"
          disabled={cancelling}
          data-testid="cancel-parse-btn"
          onClick={() => {
            setCancelling(true);
            void actions.cancelParse(entry.file_id).catch(() => setCancelling(false));
          }}
        >
          {cancelling ? t('workbench.files.cancelling_parse') : t('workbench.files.cancel_parse')}
        </button>
      )}

      {showChoice && (
        <div className="file-entry__choice" data-testid="plugin-choice">
          {entry.candidate_plugins.map((c: PluginMatch) => (
            <button
              key={c.plugin_id}
              type="button"
              className="file-entry__choice-btn"
              onClick={() => switchPlugin(c.plugin_id)}
            >
              {c.plugin_id}（{confidencePercent(c.confidence)}%）
            </button>
          ))}
        </div>
      )}

      <div className="file-entry__actions">
        {entry.status !== 'error' && (
          <button type="button" className="file-entry__btn" onClick={toggleDisabled}>
            {disabled ? t('workbench.files.enable') : t('workbench.files.disable')}
          </button>
        )}
        {entry.status === 'error' && (
          <button
            type="button"
            className="file-entry__btn"
            onClick={() => void actions.importFiles([entry.path])}
            data-testid="retry-btn"
          >
            {t('workbench.files.retry')}
          </button>
        )}
        {!showChoice && entry.candidate_plugins.length > 0 && entry.status !== 'error' && (
          <select
            className="file-entry__select"
            aria-label={t('workbench.files.choose_plugin')}
            value=""
            onChange={(e) => {
              if (e.target.value) switchPlugin(e.target.value);
            }}
          >
            <option value="">{t('workbench.files.choose_plugin')}</option>
            {entry.candidate_plugins.map((c) => (
              <option key={c.plugin_id} value={c.plugin_id}>
                {c.plugin_id}（{confidencePercent(c.confidence)}%）
              </option>
            ))}
          </select>
        )}
        <button
          type="button"
          className="file-entry__btn file-entry__btn--danger"
          onClick={() => void actions.unloadFile(entry.file_id)}
          data-testid="unload-btn"
        >
          {t('workbench.files.unload')}
        </button>
      </div>
    </li>
  );
}

/** Left-panel file lifecycle: import (OS drag&drop + file picker + mock path input), progress, enable/disable, plugin switch, unload, retry (§4.2). */
export default function FilePanel() {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  const [paths, setPaths] = useState('');
  const [dragging, setDragging] = useState(false);
  /** Call-level import failure (invoke rejected); per-file errors render on the entries instead. */
  const [importError, setImportError] = useState<string | null>(null);
  const mock = useMockIpc();

  const submitPaths = (raw: string) => {
    const list = raw
      .split(/[\r\n,]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (list.length > 0) void runImport(list);
    setPaths('');
  };

  const runImport = useCallback(
    async (list: string[], overrides?: Record<string, { plugin_id: string }>) => {
      setImportError(null);
      try {
        await actions.importFiles(list, overrides);
      } catch (e) {
        // 与 onPickFiles 同策略：ACL/原生拒绝可能是纯字符串，透传原始文本（任务 15 缺陷 4）。
        const raw =
          typeof e === 'string'
            ? e
            : e && typeof e === 'object' && 'message' in e
              ? String((e as { message: unknown }).message)
              : '';
        setImportError(t('workbench.files.import_failed', { message: raw || t('common.error.internal') }));
      }
    },
    [actions, t],
  );

  // OS drag&drop (primary production entry): Tauri 2 intercepts OS drops, so HTML5 drop
  // events never carry paths — subscribe to the core drag events instead (real mode only).
  useEffect(() => {
    if (mock) return;
    const unDrop = ipc.listen<OsDragDropPayload>(EV_OS_DRAG_DROP, (payload) => {
      setDragging(false);
      if (payload.paths.length > 0) void runImport(payload.paths);
    });
    const unEnter = ipc.listen(EV_OS_DRAG_ENTER, () => setDragging(true));
    const unLeave = ipc.listen(EV_OS_DRAG_LEAVE, () => setDragging(false));
    return () => {
      unDrop();
      unEnter();
      unLeave();
    };
  }, [mock, runImport]);

  const onPickFiles = async () => {
    try {
      const picked = await pickImportFiles();
      if (picked.length > 0) await runImport(picked);
    } catch (e) {
      // Tauri ACL/原生拒绝常以纯字符串形式到达（如 "Command ... not allowed by ACL"），
      // 透传原始文本而非吞成"内部错误"（任务 15 缺陷 4）。
      const raw =
        typeof e === 'string'
          ? e
          : e && typeof e === 'object' && 'message' in e
            ? String((e as { message: unknown }).message)
            : '';
      setImportError(t('workbench.files.pick_failed', { message: raw || t('common.error.internal') }));
    }
  };

  return (
    <section className="file-panel">
      <h2 className="file-panel__title">{t('workbench.files.title')}</h2>

      <div
        className={`file-panel__dropzone${dragging ? ' file-panel__dropzone--over' : ''}`}
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragging(false);
          // Mock mode only: HTML5 drop yields names without paths; production drops
          // are intercepted by Tauri and arrive through the tauri://drag-drop listener.
          if (mock) {
            const names = [...e.dataTransfer.files].map((f) => f.name);
            if (names.length > 0) void runImport(names);
          }
        }}
        data-testid="dropzone"
      >
        <span>{t('workbench.files.drop_hint')}</span>
        {!mock && (
          <button type="button" className="file-panel__btn file-panel__pick" onClick={() => void onPickFiles()} data-testid="pick-files-btn">
            {t('workbench.files.pick_files')}
          </button>
        )}
      </div>

      {importError && (
        <div className="file-panel__error" role="alert" data-testid="import-error">
          {importError}
        </div>
      )}

      {mock && (
        <div className="file-panel__import">
          <input
            type="text"
            className="file-panel__input"
            placeholder={t('workbench.files.path_placeholder')}
            value={paths}
            onChange={(e) => setPaths(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') submitPaths(paths);
            }}
            aria-label={t('workbench.files.path_placeholder')}
            data-testid="path-input"
          />
          <button type="button" className="file-panel__btn" onClick={() => submitPaths(paths)} disabled={!paths.trim()}>
            {t('workbench.files.import_button')}
          </button>
        </div>
      )}

      {state.files.length === 0 ? (
        <p className="file-panel__empty">{t('workbench.files.empty')}</p>
      ) : (
        <ul className="file-panel__list">
          {state.files.map((entry) => (
            <FileEntry
              key={entry.file_id}
              entry={entry}
              disabled={state.disabledFiles.has(entry.file_id)}
              progressPercent={state.progress[entry.file_id]?.percent}
              progressRecords={state.progress[entry.file_id]?.records_so_far}
            />
          ))}
        </ul>
      )}
    </section>
  );
}
