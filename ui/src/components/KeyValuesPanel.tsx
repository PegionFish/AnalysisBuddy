/** ui/src/components/KeyValuesPanel.tsx — right-panel property grid at cursor T (ipc-ui.md §4.5).
 *  Consumption semantics: the cursor query is debounced (200ms) and seq-guarded by the session provider;
 *  key_values_at never rejects — per-file errors render as placeholders with a single-file retry.
 *  Empty-results semantics: a plugin that resolves with 0 entries (e.g. builtin-csv on a file without
 *  key-values) renders a friendly per-group hint instead of an empty Key/Value/Unit table header. */

import { useTranslation } from 'react-i18next';
import type { ImportResult, KeyValueResult } from '../ipc/types';
import { useSession } from '../state/session';
import './KeyValuesPanel.css';

function pluginNameOf(file: ImportResult | undefined, plugins: { id: string; display_name: string }[]): string {
  const pluginId = file?.matched_plugin?.plugin_id;
  if (!pluginId) return '—';
  return plugins.find((p) => p.id === pluginId)?.display_name ?? pluginId;
}

function FileGroup({ result, file }: { result: KeyValueResult; file: ImportResult | undefined }) {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  const code = result.error?.code;
  const retryable = code === 'timeout' || code === 'plugin_crashed';

  return (
    <section className="kv-group" data-testid="kv-group" data-file-id={result.file_id}>
      <header className="kv-group__head">
        <span className="kv-group__file" title={file?.path}>
          {file?.name ?? result.file_id}
        </span>
        <span className="kv-group__plugin">{pluginNameOf(file, state.plugins)}</span>
      </header>

      {result.entries ? (
        <>
          <div className="kv-group__status">
            {t('workbench.keyvalues.entries_count', { count: result.entries.length })}
          </div>
          {result.entries.length === 0 ? (
            <p className="kv-group__empty" data-testid="kv-empty">
              {t('workbench.keyvalues.empty', {
                defaultValue: 'The plugin did not provide key values',
              })}
            </p>
          ) : (
            <table className="kv-grid">
              <thead>
                <tr>
                  <th>{t('workbench.keyvalues.key')}</th>
                  <th>{t('workbench.keyvalues.value')}</th>
                  <th>{t('workbench.keyvalues.unit')}</th>
                </tr>
              </thead>
              <tbody>
                {result.entries.map((entry) => (
                  <tr key={entry.key}>
                    <td className="kv-grid__key">{entry.key}</td>
                    <td className="kv-grid__value">{String(entry.value)}</td>
                    <td className="kv-grid__unit">{entry.unit ?? ''}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      ) : (
        <div className="kv-group__error" data-testid="kv-group-error">
          <span>
            {code === 'timeout'
              ? t('workbench.keyvalues.timeout')
              : t(`common.error.${code ?? ''}`, { defaultValue: result.error?.message ?? '' })}
          </span>
          {retryable && (
            <button
              type="button"
              className="kv-group__retry"
              onClick={() => actions.retryKeyValues(result.file_id)}
              data-testid="kv-retry"
            >
              {t('workbench.keyvalues.retry')}
            </button>
          )}
        </div>
      )}
    </section>
  );
}

/** Property grid for the cursor moment T: grouped per file, partial failures isolated per group (§4.5). */
export default function KeyValuesPanel() {
  const { state } = useSession();
  const { t } = useTranslation();

  if (state.cursorMs === null) {
    return (
      <section className="panel kv-panel" data-testid="keyvalues-panel">
        <h2 className="kv-panel__title">{t('workbench.keyvalues.title')}</h2>
        <p className="kv-panel__empty">{t('workbench.keyvalues.no_cursor')}</p>
      </section>
    );
  }

  const fileById = new Map(state.files.map((f) => [f.file_id, f]));

  return (
    <section className="panel kv-panel" data-testid="keyvalues-panel">
      <h2 className="kv-panel__title">{t('workbench.keyvalues.title')}</h2>
      {state.keyValues.length === 0 && state.keyValuesPending && (
        <p className="kv-panel__empty" data-testid="kv-loading">
          {t('workbench.keyvalues.loading')}
        </p>
      )}
      {state.keyValues.map((result) => (
        <FileGroup key={result.file_id} result={result} file={fileById.get(result.file_id)} />
      ))}
    </section>
  );
}
