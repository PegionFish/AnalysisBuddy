import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMockIpc } from '../ipc/ipc';
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
          {entry.status === 'parsing' && progressPercent !== undefined
            ? t('workbench.files.status_parsing_percent', { percent: progressPercent })
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

/** Left-panel file lifecycle: import (button + drag&drop), progress, enable/disable, plugin switch, unload, retry (§4.2). */
export default function FilePanel() {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  const [paths, setPaths] = useState('');
  const [dragging, setDragging] = useState(false);
  const mock = useMockIpc();

  const submitPaths = (raw: string) => {
    const list = raw
      .split(/[\r\n,]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (list.length > 0) void actions.importFiles(list);
    setPaths('');
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
          const names = [...e.dataTransfer.files].map((f) => f.name);
          if (names.length > 0) void actions.importFiles(names);
        }}
        data-testid="dropzone"
      >
        {t('workbench.files.drop_hint')}
      </div>

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
