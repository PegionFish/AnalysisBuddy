/** ui/src/ipc/ipc.ts — Ipc facade: interface + environment switch + singleton export (ipc-ui.md §3.2).
 *  UI components and the state layer import ONLY this module from src/ipc; never a concrete implementation. */

import type {
  ImportResult,
  KeyValueResult,
  LoadResult,
  MetricNode,
  PluginInfo,
  QuerySeriesArgs,
  SeriesSlice,
  SessionMeta,
} from './types';
import type { PluginLogPayload } from './events';
import { createMockIpc } from './mock';
import { createRealIpc } from './real';

export interface Ipc {
  list_plugins(): Promise<PluginInfo[]>;
  import_files(args: { paths: string[]; overrides?: Record<string, { plugin_id: string }> }): Promise<ImportResult[]>;
  unload_file(args: { file_id: string }): Promise<void>;
  get_metrics(args: { file_ids?: string[] }): Promise<MetricNode[]>;
  query_series(args: QuerySeriesArgs): Promise<SeriesSlice[]>;
  key_values_at(args: { file_ids: string[]; timestamp_ms: number }): Promise<KeyValueResult[]>;
  save_session(args: { path?: string }): Promise<SessionMeta>;
  load_session(args: { path: string }): Promise<LoadResult>;
  get_plugin_log(args: { plugin_id: string; limit?: number }): Promise<PluginLogPayload[]>;
  /** Auxiliary command (ipc-ui.md §4.6): rebuild the plugin instance; resolves with the fresh PluginInfo. */
  reload_plugin(args: { plugin_id: string }): Promise<PluginInfo>;
  /** Subscribe to an event channel; returns the unsubscribe function (same signature for mock and real). */
  listen<T>(channel: string, cb: (payload: T) => void): () => void;
}

/** Environment switch: dev-mode bare runs default to mock; VITE_AB_IPC=mock/real force a side. */
export function useMockIpc(): boolean {
  return import.meta.env.MODE === 'development' || import.meta.env.VITE_AB_IPC === 'mock';
}

export const ipc: Ipc = useMockIpc() ? createMockIpc() : createRealIpc();
