/** ui/src/ipc/fixtures/plugins.ts — fake plugin registry for the mock IPC layer (ipc-ui.md §3.3).
 *  Two plugins: builtin-csv and demo-tool. */

import type { PluginInfo, PluginMatch } from '../types';

export const PLUGIN_INFO: PluginInfo[] = [
  {
    id: 'builtin-csv',
    display_name: 'Builtin CSV',
    version: '1.0.0',
    state: 'ready',
    loaded_file_ids: [],
    capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
    last_error: null,
  },
  {
    id: 'demo-tool',
    display_name: 'Demo Tool',
    version: '0.4.2',
    state: 'ready',
    loaded_file_ids: [],
    capabilities: { annotate: true, subscribe: true, binary_sidecar: false },
    last_error: null,
  },
];

/** Suffix-based claiming rule (mock only): `.csv` → builtin-csv, anything else → demo-tool. */
export function matchPlugin(path: string): PluginMatch[] {
  const lower = path.toLowerCase();
  if (lower.endsWith('.csv')) {
    return [
      { plugin_id: 'builtin-csv', confidence: 0.97, reason: 'extension .csv' },
      { plugin_id: 'demo-tool', confidence: 0.72, reason: 'generic fallback' },
    ];
  }
  return [
    { plugin_id: 'demo-tool', confidence: 0.92, reason: 'generic parser' },
    { plugin_id: 'builtin-csv', confidence: 0.55, reason: 'weak extension match' },
  ];
}

/**
 * Confidence injection for the needs_user_choice acceptance path:
 * paths containing "choice" yield a top-two gap <0.1 (0.82 vs 0.74) so auto-match is unreliable.
 */
export function matchPluginWithChoiceInjection(path: string): PluginMatch[] {
  if (path.toLowerCase().includes('choice')) {
    return [
      { plugin_id: 'demo-tool', confidence: 0.82, reason: 'tie candidates (mock injection)' },
      { plugin_id: 'builtin-csv', confidence: 0.74, reason: 'tie candidates (mock injection)' },
    ];
  }
  return matchPlugin(path);
}

export function pluginById(id: string): PluginInfo | undefined {
  return PLUGIN_INFO.find((p) => p.id === id);
}
