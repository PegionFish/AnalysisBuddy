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
}

export interface PluginMatch {
  plugin_id: string;
  /** [0,1], from can_handle. */
  confidence: number;
  reason?: string;
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
  reason: 'not_found' | 'hash_mismatch';
}

export interface LoadResult {
  session: SessionMeta;
  /** Files that passed validation and re-enter the import pipeline. */
  loaded_file_ids: string[];
  /** Missing/validation-failed files (UI marks them). */
  missing: MissingFileEntry[];
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
