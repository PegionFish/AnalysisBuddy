/** ui/src/ipc/types.ts — the single source of truth for IPC contract types (ipc-ui.md §1.0). */

/** Unified error shape: every failing command rejects with this structure. */
export interface IpcError {
  /** Host-side error code, values per the error table in ipc-ui.md §1.0. */
  code: string;
  /** English phrase summary, e.g. "file not found". */
  message: string;
  /** Optional extra info: file_id, plugin_id, stderr digest, path, etc. */
  data?: unknown;
}

export type PluginState =
  | 'discovered'
  | 'spawning'
  | 'initializing'
  | 'ready'
  | 'loading'
  | 'parsing'
  | 'draining'
  | 'shutdown'
  | 'crashed'
  | 'timeout';

export interface PluginInfo {
  /** Same id as the plugin manifest and initialize. */
  id: string;
  display_name: string;
  /** semver */
  version: string;
  /** Current state-machine state (lowercase mapping). */
  state: PluginState;
  /** Files currently resident in this plugin. */
  loaded_file_ids: string[];
  capabilities: { annotate: boolean; subscribe: boolean; binary_sidecar: boolean };
  /** Recent failure digest (non-empty when crashed/timeout). */
  last_error: string | null;
  /** Manifest update_url (GitHub repo); absent = no update channel (§4.1, task 5/6). */
  update_url?: string;
  /** Discovery source: portable/user install dirs, or "invalid" for invalid/shadowed modules. */
  source: 'portable' | 'user' | 'invalid';
  /** Builtin modules (BUILTIN_PLUGIN_IDS) ship with the app: no uninstall/overwrite (module_protected). */
  builtin: boolean;
  /** Disabled via the module state file (.ab-modules.json); the row shows 启用. */
  disabled: boolean;
  /** Manifest author (optional; host DTO 未扩展时缺失, details panel 隐藏该行). */
  author?: string;
  /** Manifest repository https URL (optional, same gap as author). */
  repository?: string;
  /** Manifest tools constraints, e.g. "AnalysisBuddy >= 0.2.0" (optional). */
  tools?: string[];
  /** Manifest changelog entries (optional; rendered semver-desc in 版本历史, spec §6.2). */
  changelog?: ChangelogEntry[];
}

/** Manifest changelog entry (spec §3.1): version is semver, date is YYYY-MM-DD. */
export interface ChangelogEntry {
  version: string;
  date: string;
  notes: string[];
}

/** check_plugin_update result (spec §4.3). */
export interface UpdateInfo {
  plugin_id: string;
  current_version: string;
  /** Absent when the release has no usable version/asset. */
  latest_version?: string;
  is_newer: boolean;
  /** GitHub asset filename (e.g. "my-plugin-v1.2.0.zip"). */
  asset_name?: string;
}

export interface PluginMatch {
  plugin_id: string;
  /** [0,1], from can_handle. */
  confidence: number;
  reason?: string;
}

/** 数据时间范围（UTC 毫秒闭区间；protocol-v1 §2.3 `TimeRange` 的 DTO 透传，任务 19 视口自动适配消费）。 */
export interface TimeRange {
  start_ms: number;
  end_ms: number;
}

/** LoadResult 逐文件时间范围（任务 19：会话重开后视口适配）。 */
export interface FileTimeRange extends TimeRange {
  file_id: string;
}

/** 图表视图状态快照（契约 C1.4；`time_range` 省略/null = 全量）。 */
export interface ChartViewStateDto {
  time_range?: TimeRange | null;
  legend_disabled: string[];
  y_axis_scale: 'shared' | 'per_series';
}

/** 会话快照（契约 C1.4）：随 save_session 提交、load_session 读回（文件内无快照时省略键）。 */
export interface SessionSnapshot {
  /** file_id → metric 复合 id（`file_id:plugin_id:metric_id`）列表。 */
  selected_metrics: Record<string, string[]>;
  chart_view_state?: ChartViewStateDto;
  cursor_ms?: number | null;
}

/** File state after import: matched=matched awaiting parse | parsing | ready=queryable | error=retryable. */
export interface ImportResult {
  /** Host-assigned UUID v4. */
  file_id: string;
  path: string;
  name: string;
  size_bytes: number;
  status: 'matched' | 'parsing' | 'ready' | 'error';
  /** Auto-selected highest-confidence plugin; null when needs_user_choice. */
  matched_plugin: PluginMatch | null;
  /** All claiming plugins (incl. matched), sorted by confidence desc. */
  candidate_plugins: PluginMatch[];
  /** Candidate confidence gap <0.1 → auto-match unreliable, manual pick required before load/parse. */
  needs_user_choice?: boolean;
  error?: IpcError;
  /** Ready 文件的实际数据时间域（任务 19：视口自动适配；非 ready/未知省略）。 */
  time_range?: TimeRange;
}

/** Three-level tree node from get_metrics (file → plugin → metric). */
export interface MetricNode {
  level: 'file' | 'plugin' | 'metric';
  /** file: file_id; plugin: plugin_id; metric: `${file_id}:${plugin_id}:${metric_id}`. */
  id: string;
  file_id: string;
  plugin_id?: string;
  /** metric level only. */
  metric_id?: string;
  name: string;
  unit?: string;
  description?: string;
  aggregation?: 'last' | 'sum' | 'avg' | 'min' | 'max';
  children?: MetricNode[];
}

export interface SeriesPoint {
  t_ms: number;
  v: number;
}

export interface SeriesSlice {
  file_id: string;
  plugin_id: string;
  metric_id: string;
  /** Point count in this series (informational; downsampling decided by `downsampled`). */
  point_count: number;
  /** Whether downsampling occurred: passthrough of pipeline result. */
  downsampled: boolean;
  points: SeriesPoint[];
}

export interface KeyValueEntry {
  key: string;
  value: string | number | boolean;
  unit?: string;
}

/** Per-file result of key_values_at; success and failure share the same shape, partial failures do not reject. */
export interface KeyValueResult {
  file_id: string;
  /** Present on success. */
  entries?: KeyValueEntry[];
  /** Present on failure (per-file timeout/crash/not ready). */
  error?: IpcError;
}

export interface SessionMeta {
  /** Actual .absession absolute path written. */
  path: string;
  /** UTC milliseconds. */
  saved_at_ms: number;
  file_count: number;
  selected_metric_count: number;
}

export interface MissingFileEntry {
  path: string;
  reason: 'not_found' | 'hash_mismatch' | 'reopen_failed';
}

export interface LoadResult {
  session: SessionMeta;
  /** Files that passed validation and re-enter the import pipeline. */
  loaded_file_ids: string[];
  /** Missing/validation-failed files (UI marks them). */
  missing: MissingFileEntry[];
  /** 重开失败（未达 Ready）文件（UI 提示；无则省略）。 */
  reopen_failed?: MissingFileEntry[];
  /** 重开成功文件的实际数据时间域（任务 19：视口自动适配；无则省略）。 */
  time_ranges?: FileTimeRange[];
  /** 会话文件内保存的快照（契约 C1.3；旧文件/无快照时省略键）。 */
  snapshot?: SessionSnapshot;
  /**
   * 重开成功文件的完整 ImportResult（P0-01）：后端已 await 完整重放，前端
   * 直接写终态，不依赖重放进度事件的到达时序（真实 Tauri 事件在响应前
   * 已发出，占位行挂载后收不到）。无则省略键。
   */
  files?: ImportResult[];
}

/** query_series arguments (ipc-ui.md §1.5). */
export interface QuerySeriesArgs {
  file_ids: string[];
  /** Composite metric ids: file_id:plugin_id:metric_id. */
  metrics: string[];
  t0_ms: number;
  t1_ms: number;
  max_points_per_series: number;
}

export type Lang = 'zh' | 'en';
export type Theme = 'light' | 'dark';
