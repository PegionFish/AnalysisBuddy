/** ui/src/ipc/fixtures/plugins.ts — fake plugin registry for the mock IPC layer (ipc-ui.md §3.3).
 *  Two plugins: builtin-csv and demo-tool. FIXTURE_PLUGIN enters the list only via
 *  install_plugin_zip (mock install simulation, spec §4.1). */

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
    source: 'portable',
    builtin: true,
    disabled: false,
    /** 预设示例（manifest presets 透传形状）：1 条带 want + 1 条无 want、2 个 groups。 */
    presets: [
      {
        id: 'csv-metrics',
        name: { zh: 'CSV 指标预设', en: 'CSV Metrics' },
        description: { zh: '常用 CSV 指标快速载入', en: 'Common CSV metrics for quick loading' },
        entries: [
          { want: 'fps', names: ['fps', 'frame_rate'] },
          { names: ['timestamp', 'frame_ms'] },
        ],
        groups: [
          {
            id: 'performance',
            name: { zh: '性能', en: 'Performance' },
            entries: [{ want: 'fps', names: ['fps', 'frame_ms'] }],
          },
          {
            id: 'timing',
            name: { zh: '计时', en: 'Timing' },
            entries: [{ names: ['timestamp', 'time_ms'] }],
          },
        ],
        keywords: ['csv', 'metrics', 'performance'],
      },
    ],
  },
  {
    id: 'demo-tool',
    display_name: 'Demo Tool',
    version: '0.4.2',
    state: 'ready',
    loaded_file_ids: [],
    capabilities: { annotate: true, subscribe: true, binary_sidecar: false },
    last_error: null,
    source: 'portable',
    builtin: false,
    disabled: false,
  },
];

/** 模拟 install_plugin_zip/update_plugin 安装的第三方模块（spec §3.1 元信息全量）。
 *  changelog 故意乱序存放——UI 渲染必须按 semver 降序重排（spec §6.2）。 */
export const FIXTURE_PLUGIN: PluginInfo = {
  id: 'fixture-csv',
  display_name: 'Fixture CSV Pro',
  version: '1.1.0',
  state: 'discovered',
  loaded_file_ids: [],
  capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
  last_error: null,
  update_url: 'https://github.com/fixture/fixture-csv',
  source: 'portable',
  builtin: false,
  disabled: false,
  author: 'Fixture Labs',
  repository: 'https://github.com/fixture/fixture-csv',
  tools: ['AnalysisBuddy >= 0.1.0'],
  changelog: [
    { version: '1.0.5', date: '2026-06-01', notes: [] },
    { version: '1.2.0', date: '2026-08-01', notes: ['Added: header sniffing rewrite', 'Fixed: empty-row handling'] },
    { version: '1.1.0', date: '2026-06-20', notes: ['Initial release'] },
  ],
};

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
