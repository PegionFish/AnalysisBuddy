/** ui/src/ipc/real.ts — real Tauri IPC implementation (ipc-ui.md §3.4).
 *  This is the ONLY file allowed to import @tauri-apps/api (enforced by ESLint no-restricted-imports). */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Ipc } from './ipc';
import type { IpcError } from './types';

/** Normalize rejected values (Tauri serialization boundary may yield strings) to IpcError. */
function normalizeError(e: unknown): IpcError {
  if (e && typeof e === 'object' && 'code' in e && typeof (e as { code: unknown }).code === 'string') {
    return e as IpcError;
  }
  return {
    code: 'internal',
    message: typeof e === 'string' ? e : 'internal error',
    data: e,
  };
}

export function createRealIpc(): Ipc {
  const call = <T, A>(command: string, args: A): Promise<T> => invoke<T>(command, args as never).catch((e: unknown) => {
    throw normalizeError(e);
  });

  return {
    list_plugins: () => call('list_plugins', {}),
    import_files: (args) => call('import_files', args),
    unload_file: (args) => call('unload_file', args),
    get_metrics: (args) => call('get_metrics', args),
    query_series: (args) => call('query_series', args),
    key_values_at: (args) => call('key_values_at', args),
    save_session: (args) => call('save_session', args),
    load_session: (args) => call('load_session', args),
    get_plugin_log: (args) => call('get_plugin_log', args),
    listen<T>(channel: string, cb: (payload: T) => void) {
      let unlisten: (() => void) | null = null;
      listen<T>(channel, (event) => cb(event.payload)).then((fn) => {
        unlisten = fn;
      });
      return () => unlisten?.();
    },
  };
}
